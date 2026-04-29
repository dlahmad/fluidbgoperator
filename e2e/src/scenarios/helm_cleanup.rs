use std::time::Duration;

use anyhow::{Result, bail};

use crate::command;
use crate::harness::E2eHarness;

pub async fn helm_uninstall_cleans_operator_resources(
    harness: &E2eHarness,
    bgd_names: &[&str],
) -> Result<()> {
    let cfg = harness.config.clone();
    cleanup_blue_green_deployments_created_by_suite(harness, bgd_names).await?;
    command::run(
        "helm",
        [
            "uninstall",
            "fluidbg-e2e",
            "-n",
            &cfg.system_namespace,
            "--wait",
        ],
    )?;
    for (resource, name, namespace) in [
        (
            "deployment",
            "fluidbg-operator",
            cfg.system_namespace.as_str(),
        ),
        (
            "deployment",
            "fluidbg-rabbitmq-manager",
            cfg.system_namespace.as_str(),
        ),
        ("service", "fluidbg-operator", cfg.system_namespace.as_str()),
        (
            "service",
            "fluidbg-rabbitmq-manager",
            cfg.system_namespace.as_str(),
        ),
        (
            "serviceaccount",
            "fluidbg-operator",
            cfg.system_namespace.as_str(),
        ),
        ("secret", "fluidbg-e2e-auth", cfg.system_namespace.as_str()),
        ("inceptionplugin", "http", cfg.namespace.as_str()),
        ("inceptionplugin", "rabbitmq", cfg.namespace.as_str()),
    ] {
        harness
            .kube
            .wait_deleted(resource, name, namespace, Duration::from_secs(60))
            .await?;
    }
    if harness
        .kube
        .exists("clusterrole", "fluidbg-operator", "")
        .await
    {
        bail!("clusterrole/fluidbg-operator was not removed by Helm uninstall");
    }
    if harness
        .kube
        .exists("clusterrolebinding", "fluidbg-operator", "")
        .await
    {
        bail!("clusterrolebinding/fluidbg-operator was not removed by Helm uninstall");
    }
    Ok(())
}

async fn cleanup_blue_green_deployments_created_by_suite(
    harness: &E2eHarness,
    bgd_names: &[&str],
) -> Result<()> {
    let cfg = harness.config.clone();
    for bgd in bgd_names {
        harness
            .kube
            .delete_named("bluegreendeployment", bgd, &cfg.namespace)
            .await?;
    }
    for bgd in bgd_names {
        harness
            .kube
            .wait_deleted(
                "bluegreendeployment",
                bgd,
                &cfg.namespace,
                Duration::from_secs(90),
            )
            .await?;
    }
    if !harness.kube.exists("namespace", &cfg.namespace, "").await {
        return Ok(());
    }
    harness
        .kube
        .wait_no_inception_resources(&cfg.namespace)
        .await?;
    harness
        .kube
        .delete_labeled_resources(&cfg.namespace, "fluidbg.io/name=order-processor")
        .await?;
    harness
        .kube
        .wait_label_resource_count(
            &cfg.namespace,
            "fluidbg.io/name=order-processor",
            0,
            Duration::from_secs(90),
        )
        .await
}
