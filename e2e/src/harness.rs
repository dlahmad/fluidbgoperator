use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::command;
use crate::config::{E2eConfig, StateStore};
use crate::kube::Kube;
use crate::rabbitmq::RabbitMq;

pub struct E2eHarness {
    pub config: E2eConfig,
    pub kube: Kube,
    pub rabbitmq: RabbitMq,
}

impl E2eHarness {
    pub async fn setup() -> Result<Self> {
        let config = E2eConfig::from_env()?;
        verify_commands(config.build_images)?;
        let kube = Kube::new().await?;

        regenerate_crds(&config)?;
        if config.build_images {
            build_and_load_images(&config)?;
        }
        deploy_infrastructure(&config, &kube).await?;
        reset_previous_resources(&config, &kube).await?;
        install_operator(&config, &kube).await?;

        Ok(Self {
            rabbitmq: RabbitMq::new(config.system_namespace.clone()),
            config,
            kube,
        })
    }
}

fn verify_commands(build_images: bool) -> Result<()> {
    command::output("kubectl", ["version", "--client"])
        .context("missing required command: kubectl")?;
    command::output("helm", ["version", "--short"]).context("missing required command: helm")?;
    command::require("cargo")?;
    if build_images {
        command::require("docker")?;
    }
    Ok(())
}

fn regenerate_crds(config: &E2eConfig) -> Result<()> {
    command::run_in(
        &config.root_dir,
        "cargo",
        ["run", "-p", "fluidbg-operator", "--bin", "gen-crds"],
    )?;
    std::fs::copy(
        config.root_dir.join("crds/blue_green_deployment.yaml"),
        config
            .root_dir
            .join("charts/fluidbg-operator/crds/blue_green_deployment.yaml"),
    )
    .context("copy BlueGreenDeployment CRD into chart")?;
    std::fs::copy(
        config.root_dir.join("crds/inception_plugin.yaml"),
        config
            .root_dir
            .join("charts/fluidbg-operator/crds/inception_plugin.yaml"),
    )
    .context("copy InceptionPlugin CRD into chart")?;
    Ok(())
}

fn build_and_load_images(config: &E2eConfig) -> Result<()> {
    let arch = target_arch();
    let operator_image = format!("fluidbg/fbg-operator:{}", config.image_tag);
    let http_plugin_image = format!("fluidbg/fbg-plugin-http:{}", config.image_tag);
    let rabbitmq_plugin_image = format!("fluidbg/fbg-plugin-rabbitmq:{}", config.image_tag);
    let rabbitmq_infra_image = "rabbitmq:4.2-management-alpine";
    prefetch_linux_rust_dependencies(config, &arch)?;
    command::run(
        &config
            .root_dir
            .join("scripts/build-linux-binaries.sh")
            .to_string_lossy(),
        ["--arch", &arch],
    )?;

    let root = config.root_dir.to_string_lossy();
    command::run(
        "docker",
        [
            "build",
            "--platform",
            &format!("linux/{arch}"),
            "-t",
            &operator_image,
            &root,
        ],
    )?;
    command::run(
        "docker",
        [
            "build",
            "--platform",
            &format!("linux/{arch}"),
            "-f",
            &config
                .root_dir
                .join("plugins/http/Dockerfile")
                .to_string_lossy(),
            "-t",
            &http_plugin_image,
            &root,
        ],
    )?;
    command::run(
        "docker",
        [
            "build",
            "--platform",
            &format!("linux/{arch}"),
            "-f",
            &config
                .root_dir
                .join("plugins/rabbitmq/Dockerfile")
                .to_string_lossy(),
            "-t",
            &rabbitmq_plugin_image,
            &root,
        ],
    )?;
    command::run(
        "docker",
        [
            "build",
            "-t",
            "fluidbg/blue-app:dev",
            &config.root_dir.join("e2e/blue-app").to_string_lossy(),
        ],
    )?;
    command::run(
        "docker",
        [
            "build",
            "-t",
            "fluidbg/green-app:dev",
            &config.root_dir.join("e2e/green-app").to_string_lossy(),
        ],
    )?;
    command::run(
        "docker",
        [
            "build",
            "-t",
            "fluidbg/test-app:dev",
            &config.root_dir.join("e2e/test-app").to_string_lossy(),
        ],
    )?;
    build_single_platform_alias(rabbitmq_infra_image, rabbitmq_infra_image, &arch)?;
    if config.state_store == StateStore::Postgres {
        command::run(
            "docker",
            [
                "pull",
                "--platform",
                &format!("linux/{arch}"),
                "postgres:18-alpine",
            ],
        )?;
    }

    if let Some(cluster) = kind_cluster(config)? {
        for image in [
            operator_image.as_str(),
            http_plugin_image.as_str(),
            rabbitmq_plugin_image.as_str(),
            "fluidbg/blue-app:dev",
            "fluidbg/green-app:dev",
            "fluidbg/test-app:dev",
            rabbitmq_infra_image,
        ] {
            command::run("kind", ["load", "docker-image", image, "--name", &cluster])?;
        }
        if config.state_store == StateStore::Postgres {
            let _ = command::run(
                "kind",
                [
                    "load",
                    "docker-image",
                    "postgres:18-alpine",
                    "--name",
                    &cluster,
                ],
            );
        }
    }
    Ok(())
}

fn build_single_platform_alias(source: &str, tag: &str, arch: &str) -> Result<()> {
    let dir = std::env::temp_dir().join(format!(
        "fluidbg-e2e-image-{}-{}",
        std::process::id(),
        tag.replace(['/', ':'], "-")
    ));
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let dockerfile = dir.join("Dockerfile");
    std::fs::write(&dockerfile, format!("FROM {source}\n"))
        .with_context(|| format!("write {}", dockerfile.display()))?;
    let build_result = command::run(
        "docker",
        [
            "build",
            "--platform",
            &format!("linux/{arch}"),
            "-t",
            tag,
            &dir.to_string_lossy(),
        ],
    );
    let cleanup_result = std::fs::remove_dir_all(&dir)
        .with_context(|| format!("remove temporary image context {}", dir.display()));
    build_result?;
    cleanup_result?;
    Ok(())
}

async fn deploy_infrastructure(config: &E2eConfig, kube: &Kube) -> Result<()> {
    kube.apply_namespace(&config.system_namespace).await?;
    kube.apply_namespace(&config.namespace).await?;
    kube.apply_file(&config.deploy_file("infra/httpbin.yaml"))
        .await?;
    kube.apply_file(&config.deploy_file("infra/rabbitmq.yaml"))
        .await?;
    if config.state_store == StateStore::Postgres {
        kube.apply_file(&config.deploy_file("infra/postgres.yaml"))
            .await?;
    }

    kube.reset_deployment(&config.system_namespace, "rabbitmq", "rabbitmq")
        .await?;
    kube.rollout_status(
        "rabbitmq",
        &config.system_namespace,
        Duration::from_secs(120),
    )
    .await?;
    kube.rollout_status(
        "httpbin",
        &config.system_namespace,
        Duration::from_secs(120),
    )
    .await?;
    if config.state_store == StateStore::Postgres {
        kube.rollout_status(
            "postgres",
            &config.system_namespace,
            Duration::from_secs(120),
        )
        .await?;
    }
    tokio::time::sleep(Duration::from_secs(15)).await;
    Ok(())
}

async fn reset_previous_resources(config: &E2eConfig, kube: &Kube) -> Result<()> {
    kube.cleanup_stale_blue_green_deployments().await?;
    for release in ["fluidbg-e2e", "fluidbg"] {
        let _ = command::run(
            "helm",
            [
                "uninstall",
                release,
                "-n",
                &config.system_namespace,
                "--ignore-not-found",
                "--no-hooks",
                "--wait",
            ],
        );
    }
    kube.delete_named(
        "job",
        "fluidbg-operator-builtin-plugin-hook-apply",
        &config.system_namespace,
    )
    .await?;
    kube.delete_named(
        "job",
        "fluidbg-operator-builtin-plugin-hook-delete",
        &config.system_namespace,
    )
    .await?;
    kube.delete_labeled_resources(
        &config.system_namespace,
        "app.kubernetes.io/component=builtin-plugin-hook",
    )
    .await?;
    kube.delete_labeled_resources(&config.namespace, "fluidbg.io/name=order-processor")
        .await?;
    kube.delete_named(
        "configmap",
        "fluidbg-config-incoming-orders",
        &config.namespace,
    )
    .await?;
    kube.delete_named(
        "configmap",
        "fluidbg-config-outgoing-results",
        &config.namespace,
    )
    .await?;
    kube.delete_named("deployment", "test-container", &config.namespace)
        .await?;
    kube.delete_named("service", "test-container", &config.namespace)
        .await?;
    kube.delete_labeled_resources(&config.namespace, "fluidbg.io/test")
        .await?;
    kube.delete_labeled_resources(&config.namespace, "fluidbg.io/inception-point")
        .await?;
    if config.state_store != StateStore::Postgres {
        kube.delete_named("deployment", "postgres", &config.system_namespace)
            .await?;
        kube.delete_named("service", "postgres", &config.system_namespace)
            .await?;
        kube.delete_named("secret", "fluidbg-postgres", &config.system_namespace)
            .await?;
    }
    kube.delete_crds().await?;
    kube.wait_deleted(
        "deployment",
        "test-container",
        &config.namespace,
        Duration::from_secs(30),
    )
    .await?;
    kube.wait_deleted(
        "service",
        "test-container",
        &config.namespace,
        Duration::from_secs(30),
    )
    .await?;
    kube.wait_no_inception_resources(&config.namespace).await
}

async fn install_operator(config: &E2eConfig, kube: &Kube) -> Result<()> {
    kube.install_operator_chart(config)?;
    kube.rollout_status(
        "fluidbg-operator",
        &config.system_namespace,
        Duration::from_secs(120),
    )
    .await?;
    kube.rollout_status(
        "fluidbg-rabbitmq-manager",
        &config.system_namespace,
        Duration::from_secs(120),
    )
    .await?;
    kube.wait_exists(
        "inceptionplugin",
        "http",
        &config.namespace,
        Duration::from_secs(60),
    )
    .await?;
    kube.wait_exists(
        "inceptionplugin",
        "rabbitmq",
        &config.namespace,
        Duration::from_secs(60),
    )
    .await
}

fn target_arch() -> String {
    std::env::var("TARGET_ARCH")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            command::output(
                "kubectl",
                [
                    "get",
                    "nodes",
                    "-o",
                    "jsonpath={.items[0].status.nodeInfo.architecture}",
                ],
            )
            .ok()
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "amd64".to_string())
}

fn prefetch_linux_rust_dependencies(config: &E2eConfig, arch: &str) -> Result<()> {
    std::fs::create_dir_all(config.root_dir.join(".docker-cargo-home/registry"))?;
    std::fs::create_dir_all(config.root_dir.join(".docker-cargo-home/git"))?;
    command::run(
        "docker",
        [
            "run",
            "--rm",
            "--platform",
            &format!("linux/{arch}"),
            "-v",
            &format!("{}:/work", config.root_dir.to_string_lossy()),
            "-v",
            &format!(
                "{}:/cargo-home",
                config.root_dir.join(".docker-cargo-home").to_string_lossy()
            ),
            "-e",
            "CARGO_HOME=/cargo-home",
            "-e",
            "CARGO_HTTP_TIMEOUT=600",
            "-e",
            "CARGO_NET_RETRY=10",
            "-e",
            "CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse",
            "-w",
            "/work",
            "rust:bookworm",
            "bash",
            "-lc",
            "set -euo pipefail; export PATH=/usr/local/cargo/bin:$PATH; cargo fetch --locked",
        ],
    )
}

fn kind_cluster(config: &E2eConfig) -> Result<Option<String>> {
    if command::output_allow_failure("kind", ["version"])?.is_none() {
        return Ok(None);
    }
    if let Some(cluster) = &config.kind_cluster {
        if kind_cluster_exists(cluster)? {
            return Ok(Some(cluster.clone()));
        }
        bail!("KIND_CLUSTER={cluster} does not exist");
    }
    let clusters = command::output("kind", ["get", "clusters"])?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let cluster = if clusters.len() == 1 {
        clusters[0].clone()
    } else {
        "fluidbg-dev".to_string()
    };
    Ok(kind_cluster_exists(&cluster)?.then_some(cluster))
}

fn kind_cluster_exists(cluster: &str) -> Result<bool> {
    Ok(command::output("kind", ["get", "clusters"])?
        .lines()
        .any(|line| line.trim() == cluster))
}
