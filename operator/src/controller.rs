use std::collections::BTreeMap;
use std::sync::Arc;

use futures::StreamExt;
use k8s_openapi::api::apps::v1::{Deployment, DeploymentSpec};
use k8s_openapi::api::core::v1::{
    ConfigMap, Container, ContainerPort, EnvVar, PodSpec, PodTemplateSpec, Service, ServicePort,
    ServiceSpec,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::api::{Api, DeleteParams, ListParams, Patch, PatchParams, PostParams};
use kube::runtime::controller::Action;
use kube::runtime::{Controller, watcher};
use serde_json::json;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{error, info, warn};

use crate::crd::blue_green::{BGDPhase, BlueGreenDeployment, DeploymentRef, DeploymentSelector};
use crate::crd::inception_plugin::InceptionPlugin;
use crate::plugins::reconciler::{reconcile_inception_point, render_container_env_injections};
use crate::state_store::StateStore;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum AssignmentTarget {
    Green,
    Blue,
    Test,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum AssignmentKind {
    Env,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PropertyAssignment {
    target: AssignmentTarget,
    kind: AssignmentKind,
    name: String,
    value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    container_name: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PluginLifecycleResponse {
    #[serde(default)]
    assignments: Vec<PropertyAssignment>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct DeploymentPatchKey {
    namespace: String,
    name: String,
    container_name: Option<String>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct DeploymentIdentity {
    namespace: String,
    name: String,
}

struct Ctx {
    client: kube::Client,
    namespace: String,
    store: Arc<dyn StateStore>,
}

#[derive(Debug, Error)]
enum ReconcileError {
    #[error("store error: {0}")]
    Store(String),
    #[error("k8s error: {0}")]
    K8s(#[from] kube::Error),
}

pub async fn run_controller(client: kube::Client, namespace: String, store: Arc<dyn StateStore>) {
    let bgd_api: Api<BlueGreenDeployment> = Api::namespaced(client.clone(), &namespace);

    let ctx = Arc::new(Ctx {
        client,
        namespace,
        store,
    });

    Controller::new(bgd_api, watcher::Config::default())
        .run(reconcile, error_policy, ctx)
        .for_each(|res| async move {
            match res {
                Ok((obj, action)) => {
                    info!("reconciled {:?} -> {:?}", obj, action);
                }
                Err(e) => {
                    error!("controller error: {:?}", e);
                }
            }
        })
        .await;
}

fn error_policy(_bgd: Arc<BlueGreenDeployment>, _err: &ReconcileError, _ctx: Arc<Ctx>) -> Action {
    Action::requeue(std::time::Duration::from_secs(30))
}

async fn reconcile(
    bgd: Arc<BlueGreenDeployment>,
    ctx: Arc<Ctx>,
) -> std::result::Result<Action, ReconcileError> {
    let name = bgd
        .metadata
        .name
        .as_deref()
        .unwrap_or("unknown")
        .to_string();
    let namespace = ctx.namespace.clone();
    let client = ctx.client.clone();

    let phase = bgd
        .status
        .as_ref()
        .and_then(|s| s.phase.clone())
        .unwrap_or(BGDPhase::Pending);

    info!(
        "reconciling BlueGreenDeployment '{}' phase={:?}",
        name, phase
    );

    match phase {
        BGDPhase::Pending => {
            ensure_declared_deployments(&bgd, &client, &namespace).await?;
            ensure_inception_resources(&bgd, &client, &namespace).await?;
            ensure_test_resources(&bgd, &client, &namespace).await?;
            update_status_phase(&bgd, &client, &namespace, BGDPhase::Observing).await;
            Ok(Action::requeue(std::time::Duration::from_secs(5)))
        }
        BGDPhase::Observing => {
            let counts = ctx
                .store
                .counts(&name)
                .await
                .map_err(|e| ReconcileError::Store(e.to_string()))?;

            let total = counts.passed + counts.failed + counts.timed_out;
            let success_rate = if total > 0 {
                counts.passed as f64 / total as f64
            } else {
                0.0
            };

            let min_cases = bgd
                .spec
                .promotion
                .as_ref()
                .and_then(|p| p.success_criteria.min_cases)
                .unwrap_or(100);
            let threshold = bgd
                .spec
                .promotion
                .as_ref()
                .and_then(|p| p.success_criteria.success_rate)
                .unwrap_or(0.98);

            update_status_counts(&bgd, &client, &namespace, &counts, success_rate).await;

            if total < min_cases {
                info!(
                    "BGD '{}' observing: {}/{} cases, rate={:.4}",
                    name, total, min_cases, success_rate
                );
                return Ok(Action::requeue(std::time::Duration::from_secs(5)));
            }

            if success_rate >= threshold {
                info!(
                    "BGD '{}' promotion threshold met! rate={:.4}",
                    name, success_rate
                );
                update_status_phase(&bgd, &client, &namespace, BGDPhase::Promoting).await;
            } else {
                info!(
                    "BGD '{}' rolling back: rate={:.4} < {}",
                    name, success_rate, threshold
                );
                restore_after_rollback(&bgd, &client, &namespace).await?;
                update_status_phase(&bgd, &client, &namespace, BGDPhase::RolledBack).await;
                cleanup_test_resources(&bgd, &client, &namespace).await?;
            }
            Ok(Action::requeue(std::time::Duration::from_secs(5)))
        }
        BGDPhase::Promoting => {
            promote(&bgd, &client, &namespace).await?;
            restore_after_promotion(&bgd, &client, &namespace).await?;
            update_status_phase(&bgd, &client, &namespace, BGDPhase::Completed).await;
            cleanup_test_resources(&bgd, &client, &namespace).await?;
            Ok(Action::requeue(std::time::Duration::from_secs(300)))
        }
        BGDPhase::Completed | BGDPhase::RolledBack => {
            info!("BGD '{}' in terminal state {:?}", name, phase);
            cleanup_test_resources(&bgd, &client, &namespace).await?;
            Ok(Action::requeue(std::time::Duration::from_secs(300)))
        }
    }
}

async fn promote(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
) -> std::result::Result<(), ReconcileError> {
    let current_green = resolve_current_green(client, namespace, &bgd.spec.green.selector).await?;
    let candidate = &bgd.spec.blue.deployment;
    let candidate_namespace = deployment_namespace(candidate, namespace);

    let candidate_api: Api<Deployment> = Api::namespaced(client.clone(), &candidate_namespace);
    let mut candidate_deploy = candidate_api.get(&candidate.name).await?;
    apply_selector_labels_to_deployment(
        &mut candidate_deploy,
        &bgd.spec.green.selector.match_labels,
    )?;

    candidate_api
        .replace(&candidate.name, &Default::default(), &candidate_deploy)
        .await?;

    info!(
        "promoted: deployment '{}/{}' now matches green selector",
        candidate_namespace, candidate.name
    );

    delete_current_green(client, namespace, candidate, &current_green).await?;
    Ok(())
}

async fn ensure_declared_deployments(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
) -> std::result::Result<(), ReconcileError> {
    if let Some(manifest) = &bgd.spec.blue.manifest {
        apply_deployment_manifest(
            client,
            namespace,
            &bgd.spec.blue.deployment,
            manifest,
            Some(&bgd.spec.green.selector.match_labels),
        )
        .await?;
    }
    Ok(())
}

async fn restore_after_promotion(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
) -> std::result::Result<(), ReconcileError> {
    restore_injected_envs(bgd, client, namespace, false, true).await
}

async fn restore_after_rollback(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
) -> std::result::Result<(), ReconcileError> {
    restore_injected_envs(bgd, client, namespace, true, false).await?;

    delete_deployment(
        client,
        &deployment_namespace(&bgd.spec.blue.deployment, namespace),
        &bgd.spec.blue.deployment.name,
    )
    .await
}

async fn ensure_test_resources(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
) -> std::result::Result<(), ReconcileError> {
    for test in &bgd.spec.tests {
        let env = test
            .env
            .iter()
            .map(|env| EnvVar {
                name: env.name.clone(),
                value: Some(env.value.clone()),
                ..Default::default()
            })
            .collect::<Vec<_>>();

        let labels = BTreeMap::from([
            ("app".to_string(), test.name.clone()),
            ("fluidbg.io/test".to_string(), test.name.clone()),
        ]);
        let deployment = Deployment {
            metadata: ObjectMeta {
                name: Some(test.name.clone()),
                namespace: Some(namespace.to_string()),
                labels: Some(labels.clone()),
                ..Default::default()
            },
            spec: Some(DeploymentSpec {
                replicas: Some(1),
                selector: LabelSelector {
                    match_labels: Some(labels.clone()),
                    ..Default::default()
                },
                template: PodTemplateSpec {
                    metadata: Some(ObjectMeta {
                        labels: Some(labels.clone()),
                        ..Default::default()
                    }),
                    spec: Some(PodSpec {
                        containers: vec![Container {
                            name: test.name.clone(),
                            image: Some(test.image.clone()),
                            image_pull_policy: Some("Never".to_string()),
                            ports: Some(vec![ContainerPort {
                                container_port: test.port,
                                ..Default::default()
                            }]),
                            env: Some(env),
                            ..Default::default()
                        }],
                        termination_grace_period_seconds: Some(1),
                        ..Default::default()
                    }),
                },
                ..Default::default()
            }),
            ..Default::default()
        };
        apply_resource(
            Api::namespaced(client.clone(), namespace),
            &test.name,
            &deployment,
        )
        .await?;

        let service = Service {
            metadata: ObjectMeta {
                name: Some(test.name.clone()),
                namespace: Some(namespace.to_string()),
                labels: Some(labels.clone()),
                ..Default::default()
            },
            spec: Some(ServiceSpec {
                selector: Some(labels),
                ports: Some(vec![ServicePort {
                    port: test.port,
                    target_port: Some(IntOrString::Int(test.port)),
                    ..Default::default()
                }]),
                ..Default::default()
            }),
            ..Default::default()
        };
        apply_resource(
            Api::namespaced(client.clone(), namespace),
            &test.name,
            &service,
        )
        .await?;
    }

    Ok(())
}

async fn ensure_inception_resources(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
) -> std::result::Result<(), ReconcileError> {
    let plugins: Api<InceptionPlugin> = Api::namespaced(client.clone(), namespace);
    let operator_url = "http://fluidbg-operator.fluidbg-system:8090";
    let test_container_url = bgd
        .spec
        .tests
        .first()
        .map(|test| format!("http://{}:{}", test.name, test.port))
        .unwrap_or_else(|| "http://localhost:8080".to_string());

    for ip in &bgd.spec.inception_points {
        let plugin = plugins.get(&ip.plugin_ref.name).await?;
        let resources = reconcile_inception_point(
            &plugin,
            ip,
            namespace,
            operator_url,
            &test_container_url,
            &bgd.spec.blue.deployment.name,
            bgd.metadata.name.as_deref().unwrap_or(""),
        )
        .map_err(ReconcileError::Store)?;

        for cm in resources.config_maps {
            let name =
                cm.metadata.name.clone().ok_or_else(|| {
                    ReconcileError::Store("generated ConfigMap has no name".into())
                })?;
            apply_resource(Api::namespaced(client.clone(), namespace), &name, &cm).await?;
        }
        for deployment in resources.deployments {
            let name =
                deployment.metadata.name.clone().ok_or_else(|| {
                    ReconcileError::Store("generated Deployment has no name".into())
                })?;
            apply_resource(
                Api::namespaced(client.clone(), namespace),
                &name,
                &deployment,
            )
            .await?;
        }
        for service in resources.services {
            let name = service
                .metadata
                .name
                .clone()
                .ok_or_else(|| ReconcileError::Store("generated Service has no name".into()))?;
            apply_resource(Api::namespaced(client.clone(), namespace), &name, &service).await?;
        }

        let mut assignments = Vec::new();
        assignments.extend(resources.green_env_injections.into_iter().map(|env| PropertyAssignment {
            target: AssignmentTarget::Green,
            kind: AssignmentKind::Env,
            name: env.name,
            value: env.value.unwrap_or_default(),
            container_name: None,
        }));
        assignments.extend(resources.blue_env_injections.into_iter().map(|env| PropertyAssignment {
            target: AssignmentTarget::Blue,
            kind: AssignmentKind::Env,
            name: env.name,
            value: env.value.unwrap_or_default(),
            container_name: None,
        }));
        assignments.extend(resources.test_env_injections.into_iter().map(|env| PropertyAssignment {
            target: AssignmentTarget::Test,
            kind: AssignmentKind::Env,
            name: env.name,
            value: env.value.unwrap_or_default(),
            container_name: None,
        }));

        if let Some(mut lifecycle_assignments) =
            invoke_plugin_lifecycle(client, namespace, ip.name.as_str(), &plugin, true).await?
        {
            assignments.append(&mut lifecycle_assignments.assignments);
        }

        if !assignments.is_empty() {
            let touched = apply_assignments(bgd, client, namespace, &assignments, false).await?;
            wait_for_deployments_ready(client, &touched).await?;
        }
    }

    Ok(())
}

async fn apply_deployment_manifest(
    client: &kube::Client,
    default_namespace: &str,
    deployment_ref: &DeploymentRef,
    manifest: &Deployment,
    remove_labels: Option<&BTreeMap<String, String>>,
) -> std::result::Result<(), ReconcileError> {
    let mut deployment = manifest.clone();

    if deployment.metadata.name.is_none() {
        deployment.metadata.name = Some(deployment_ref.name.clone());
    }
    if deployment.metadata.namespace.is_none() {
        deployment.metadata.namespace =
            Some(deployment_namespace(deployment_ref, default_namespace));
    }
    if let Some(labels) = remove_labels {
        strip_labels_from_deployment(&mut deployment, labels);
    }

    let name = deployment
        .metadata
        .name
        .clone()
        .unwrap_or_else(|| deployment_ref.name.clone());
    let deploy_namespace = deployment
        .metadata
        .namespace
        .clone()
        .unwrap_or_else(|| deployment_namespace(deployment_ref, default_namespace));

    let deploy_api: Api<Deployment> = Api::namespaced(client.clone(), &deploy_namespace);
    match deploy_api.get(&name).await {
        Ok(existing) => {
            deployment.metadata.resource_version = existing.metadata.resource_version;
            deploy_api
                .replace(&name, &Default::default(), &deployment)
                .await?;
        }
        Err(kube::Error::Api(err)) if err.code == 404 => {
            deploy_api.create(&PostParams::default(), &deployment).await?;
        }
        Err(err) => return Err(ReconcileError::K8s(err)),
    }
    info!(
        "applied declared deployment '{}/{}'",
        deploy_namespace, name
    );
    Ok(())
}

fn strip_labels_from_deployment(
    deployment: &mut Deployment,
    labels_to_remove: &BTreeMap<String, String>,
) {
    if let Some(labels) = deployment.metadata.labels.as_mut() {
        for key in labels_to_remove.keys() {
            labels.remove(key);
        }
        if labels.is_empty() {
            deployment.metadata.labels = None;
        }
    }

    if let Some(template_labels) = deployment
        .spec
        .as_mut()
        .and_then(|spec| spec.template.metadata.as_mut())
        .and_then(|metadata| metadata.labels.as_mut())
    {
        for key in labels_to_remove.keys() {
            template_labels.remove(key);
        }
        if template_labels.is_empty() {
            if let Some(spec) = deployment.spec.as_mut() {
                if let Some(metadata) = spec.template.metadata.as_mut() {
                    metadata.labels = None;
                }
            }
        }
    }
}

async fn apply_resource<K>(
    api: Api<K>,
    name: &str,
    resource: &K,
) -> std::result::Result<(), ReconcileError>
where
    K: kube::Resource<DynamicType = ()>
        + Clone
        + serde::de::DeserializeOwned
        + serde::Serialize
        + std::fmt::Debug,
{
    let pp = PatchParams::apply("fluidbg-operator").force();
    api.patch(name, &pp, &Patch::Apply(resource)).await?;
    Ok(())
}

async fn patch_deployment_env(
    client: &kube::Client,
    namespace: &str,
    name: &str,
    env_overrides: &[(&str, &str)],
) -> std::result::Result<(), ReconcileError> {
    let api: Api<Deployment> = Api::namespaced(client.clone(), namespace);
    let mut deployment = api.get(name).await?;
    let spec = deployment
        .spec
        .as_mut()
        .ok_or_else(|| ReconcileError::Store(format!("deployment '{name}' has no spec")))?;
    let pod_spec = spec.template.spec.as_mut().ok_or_else(|| {
        ReconcileError::Store(format!("deployment '{name}' has no pod template spec"))
    })?;
    for container in &mut pod_spec.containers {
        let env = container.env.get_or_insert_with(Vec::new);
        for (env_name, value) in env_overrides {
            upsert_env(env, env_name, value);
        }
    }

    api.replace(name, &Default::default(), &deployment).await?;
    info!(
        "patched deployment env for '{}/{}': {:?}",
        namespace, name, env_overrides
    );
    Ok(())
}

async fn patch_deployment_env_vars(
    client: &kube::Client,
    namespace: &str,
    name: &str,
    env_overrides: &[EnvVar],
) -> std::result::Result<(), ReconcileError> {
    let pairs = env_overrides
        .iter()
        .filter_map(|env| env.value.as_deref().map(|value| (env.name.as_str(), value)))
        .collect::<Vec<_>>();
    patch_deployment_env(client, namespace, name, &pairs).await
}

async fn restore_injected_envs(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
    restore_green: bool,
    restore_blue: bool,
) -> std::result::Result<(), ReconcileError> {
    if !restore_green && !restore_blue {
        return Ok(());
    }

    let plugins: Api<InceptionPlugin> = Api::namespaced(client.clone(), namespace);
    let current_green = if restore_green {
        Some(resolve_current_green(client, namespace, &bgd.spec.green.selector).await?)
    } else {
        None
    };

    for ip in &bgd.spec.inception_points {
        let plugin = plugins.get(&ip.plugin_ref.name).await?;
        if let Some(assignments) =
            invoke_plugin_lifecycle(client, namespace, ip.name.as_str(), &plugin, false).await?
        {
            let touched =
                apply_assignments(bgd, client, namespace, &assignments.assignments, true).await?;
            wait_for_deployments_ready(client, &touched).await?;
        }
        let injections = render_container_env_injections(&plugin, ip, true);

        if restore_green && !injections.green.is_empty() {
            let green = current_green
                .as_ref()
                .ok_or_else(|| ReconcileError::Store("missing current green deployment".into()))?;
            patch_deployment_env_vars(
                client,
                &deployment_namespace(green, namespace),
                &green.name,
                &injections.green,
            )
            .await?;
            wait_for_deployment_ready(
                client,
                &DeploymentIdentity {
                    namespace: deployment_namespace(green, namespace),
                    name: green.name.clone(),
                },
            )
            .await?;
        }
        if restore_blue && !injections.blue.is_empty() {
            patch_deployment_env_vars(
                client,
                &deployment_namespace(&bgd.spec.blue.deployment, namespace),
                &bgd.spec.blue.deployment.name,
                &injections.blue,
            )
            .await?;
            wait_for_deployment_ready(
                client,
                &DeploymentIdentity {
                    namespace: deployment_namespace(&bgd.spec.blue.deployment, namespace),
                    name: bgd.spec.blue.deployment.name.clone(),
                },
            )
            .await?;
        }
    }

    Ok(())
}

async fn invoke_plugin_lifecycle(
    _client: &kube::Client,
    namespace: &str,
    inception_point: &str,
    plugin: &InceptionPlugin,
    prepare: bool,
) -> std::result::Result<Option<PluginLifecycleResponse>, ReconcileError> {
    let Some(lifecycle) = &plugin.spec.lifecycle else {
        return Ok(None);
    };
    let path = if prepare {
        lifecycle.prepare_path.as_deref()
    } else {
        lifecycle.cleanup_path.as_deref()
    };
    let Some(path) = path else {
        return Ok(None);
    };
    let service_name = format!("fluidbg-{}-svc", inception_point);
    let port = plugin
        .spec
        .container
        .ports
        .first()
        .map(|port| port.container_port)
        .unwrap_or(9090);
    let url = format!("http://{}.{}:{port}{path}", service_name, namespace);
    let http = reqwest::Client::new();

    for attempt in 1..=10 {
        match http.post(&url).send().await {
            Ok(response) => {
                let response = response.error_for_status().map_err(|e| {
                    ReconcileError::Store(format!("plugin lifecycle call failed for {url}: {e}"))
                })?;
                let payload = response.json::<PluginLifecycleResponse>().await.map_err(|e| {
                    ReconcileError::Store(format!(
                        "plugin lifecycle response deserialization failed for {url}: {e}"
                    ))
                })?;
                return Ok(Some(payload));
            }
            Err(err) if attempt < 10 => {
                warn!(
                    "plugin lifecycle endpoint {} not ready yet (attempt {}/10): {}",
                    url, attempt, err
                );
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            Err(err) => {
                return Err(ReconcileError::Store(format!(
                    "plugin lifecycle call failed for {url}: {err}"
                )));
            }
        }
    }

    Ok(None)
}

async fn apply_assignments(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
    assignments: &[PropertyAssignment],
    ignore_missing: bool,
) -> std::result::Result<Vec<DeploymentIdentity>, ReconcileError> {
    let green_ref = if assignments
        .iter()
        .any(|assignment| matches!(assignment.target, AssignmentTarget::Green))
    {
        Some(resolve_current_green(client, namespace, &bgd.spec.green.selector).await?)
    } else {
        None
    };

    let mut grouped: BTreeMap<DeploymentPatchKey, BTreeMap<String, String>> = BTreeMap::new();
    let mut touched: BTreeMap<DeploymentIdentity, ()> = BTreeMap::new();

    for assignment in assignments {
        if !matches!(assignment.kind, AssignmentKind::Env) {
            continue;
        }

        match assignment.target {
            AssignmentTarget::Green => {
                if let Some(green) = &green_ref {
                    let namespace = deployment_namespace(green, namespace);
                    let identity = DeploymentIdentity {
                        namespace: namespace.clone(),
                        name: green.name.clone(),
                    };
                    let key = DeploymentPatchKey {
                        namespace: namespace.clone(),
                        name: green.name.clone(),
                        container_name: assignment.container_name.clone(),
                    };
                    grouped
                        .entry(key)
                        .or_default()
                        .insert(assignment.name.clone(), assignment.value.clone());
                    touched.insert(identity, ());
                }
            }
            AssignmentTarget::Blue => {
                let deploy_namespace = deployment_namespace(&bgd.spec.blue.deployment, namespace);
                let identity = DeploymentIdentity {
                    namespace: deploy_namespace.clone(),
                    name: bgd.spec.blue.deployment.name.clone(),
                };
                let key = DeploymentPatchKey {
                    namespace: deploy_namespace.clone(),
                    name: bgd.spec.blue.deployment.name.clone(),
                    container_name: assignment.container_name.clone(),
                };
                grouped
                    .entry(key)
                    .or_default()
                    .insert(assignment.name.clone(), assignment.value.clone());
                touched.insert(identity, ());
            }
            AssignmentTarget::Test => {
                for test in &bgd.spec.tests {
                    let identity = DeploymentIdentity {
                        namespace: namespace.to_string(),
                        name: test.name.clone(),
                    };
                    let key = DeploymentPatchKey {
                        namespace: namespace.to_string(),
                        name: test.name.clone(),
                        container_name: assignment.container_name.clone(),
                    };
                    grouped
                        .entry(key)
                        .or_default()
                        .insert(assignment.name.clone(), assignment.value.clone());
                    touched.insert(identity, ());
                }
            }
        }
    }

    for (key, env_map) in grouped {
        let env_pairs = env_map
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect::<Vec<_>>();
        patch_targeted_deployment_env(
            client,
            &key.namespace,
            &key.name,
            key.container_name.as_deref(),
            &env_pairs,
            ignore_missing,
        )
        .await?;
    }

    Ok(touched.into_keys().collect())
}

async fn patch_targeted_deployment_env(
    client: &kube::Client,
    namespace: &str,
    name: &str,
    container_name: Option<&str>,
    env_overrides: &[(&str, &str)],
    ignore_missing: bool,
) -> std::result::Result<(), ReconcileError> {
    let api: Api<Deployment> = Api::namespaced(client.clone(), namespace);
    let mut deployment = match api.get(name).await {
        Ok(deployment) => deployment,
        Err(kube::Error::Api(err)) if err.code == 404 && ignore_missing => return Ok(()),
        Err(err) => return Err(ReconcileError::K8s(err)),
    };
    let spec = deployment
        .spec
        .as_mut()
        .ok_or_else(|| ReconcileError::Store(format!("deployment '{name}' has no spec")))?;
    let pod_spec = spec.template.spec.as_mut().ok_or_else(|| {
        ReconcileError::Store(format!("deployment '{name}' has no pod template spec"))
    })?;
    for container in &mut pod_spec.containers {
        if let Some(target_name) = container_name && container.name != target_name {
            continue;
        }
        let env = container.env.get_or_insert_with(Vec::new);
        for (env_name, value) in env_overrides {
            upsert_env(env, env_name, value);
        }
    }

    api.replace(name, &Default::default(), &deployment).await?;
    Ok(())
}

async fn wait_for_deployments_ready(
    client: &kube::Client,
    deployments: &[DeploymentIdentity],
) -> std::result::Result<(), ReconcileError> {
    for deployment in deployments {
        wait_for_deployment_ready(client, deployment).await?;
    }
    Ok(())
}

async fn wait_for_deployment_ready(
    client: &kube::Client,
    deployment: &DeploymentIdentity,
) -> std::result::Result<(), ReconcileError> {
    let api: Api<Deployment> = Api::namespaced(client.clone(), &deployment.namespace);

    for attempt in 1..=60 {
        let current = api.get(&deployment.name).await?;
        if deployment_is_ready(&current) {
            return Ok(());
        }

        warn!(
            "waiting for deployment '{}/{}' rollout (attempt {}/60)",
            deployment.namespace, deployment.name, attempt
        );
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }

    Err(ReconcileError::Store(format!(
        "deployment '{}/{}' did not become ready after rollout",
        deployment.namespace, deployment.name
    )))
}

fn deployment_is_ready(deployment: &Deployment) -> bool {
    let Some(spec) = deployment.spec.as_ref() else {
        return false;
    };
    let Some(status) = deployment.status.as_ref() else {
        return false;
    };

    let desired = spec.replicas.unwrap_or(1).max(0);
    let observed_generation = status.observed_generation.unwrap_or_default();
    let generation = deployment.metadata.generation.unwrap_or_default();
    let updated = status.updated_replicas.unwrap_or_default();
    let available = status.available_replicas.unwrap_or_default();

    observed_generation >= generation && updated >= desired && available >= desired
}

fn upsert_env(env: &mut Vec<EnvVar>, name: &str, value: &str) {
    if let Some(existing) = env.iter_mut().find(|env| env.name == name) {
        existing.value = Some(value.to_string());
        return;
    }

    env.push(EnvVar {
        name: name.to_string(),
        value: Some(value.to_string()),
        ..Default::default()
    });
}

async fn resolve_current_green(
    client: &kube::Client,
    namespace: &str,
    selector: &DeploymentSelector,
) -> std::result::Result<DeploymentRef, ReconcileError> {
    if selector.match_labels.is_empty() {
        return Err(ReconcileError::Store(
            "green.selector.matchLabels must contain at least one label".to_string(),
        ));
    }

    let green_namespace = selector
        .namespace
        .clone()
        .unwrap_or_else(|| namespace.to_string());
    let deploy_api: Api<Deployment> = Api::namespaced(client.clone(), &green_namespace);
    let deployments = deploy_api
        .list(&ListParams::default().labels(&label_selector(&selector.match_labels)))
        .await?;

    match deployments.items.as_slice() {
        [deployment] => {
            let name = deployment.metadata.name.clone().ok_or_else(|| {
                ReconcileError::Store("selected green deployment has no name".to_string())
            })?;
            Ok(DeploymentRef {
                name,
                namespace: Some(green_namespace),
            })
        }
        [] => Err(ReconcileError::Store(format!(
            "green.selector matched no deployments in namespace '{}'",
            green_namespace
        ))),
        matches => Err(ReconcileError::Store(format!(
            "green.selector matched {} deployments in namespace '{}'; expected exactly one",
            matches.len(),
            green_namespace
        ))),
    }
}

async fn delete_current_green(
    client: &kube::Client,
    namespace: &str,
    candidate_ref: &DeploymentRef,
    current_green_ref: &DeploymentRef,
) -> std::result::Result<(), ReconcileError> {
    let candidate_namespace = deployment_namespace(candidate_ref, namespace);
    let green_namespace = deployment_namespace(current_green_ref, namespace);

    if candidate_ref.name == current_green_ref.name && candidate_namespace == green_namespace {
        info!(
            "candidate and current green both reference '{}/{}'; skipping green deletion",
            candidate_namespace, candidate_ref.name
        );
        return Ok(());
    }

    let deploy_api: Api<Deployment> = Api::namespaced(client.clone(), &green_namespace);
    match deploy_api
        .delete(&current_green_ref.name, &DeleteParams::default())
        .await
    {
        Ok(_) => {
            info!(
                "deleted previous green deployment '{}/{}'",
                green_namespace, current_green_ref.name
            );
            Ok(())
        }
        Err(kube::Error::Api(e)) if e.code == 404 => Ok(()),
        Err(e) => Err(ReconcileError::K8s(e)),
    }
}

fn deployment_namespace(deployment_ref: &DeploymentRef, default_namespace: &str) -> String {
    deployment_ref
        .namespace
        .clone()
        .unwrap_or_else(|| default_namespace.to_string())
}

fn label_selector(labels: &std::collections::BTreeMap<String, String>) -> String {
    labels
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn apply_selector_labels_to_deployment(
    deployment: &mut Deployment,
    labels_to_apply: &std::collections::BTreeMap<String, String>,
) -> std::result::Result<(), ReconcileError> {
    if labels_to_apply.is_empty() {
        return Err(ReconcileError::Store(
            "green.selector.matchLabels must contain at least one label".to_string(),
        ));
    }

    let deployment_labels = deployment
        .metadata
        .labels
        .get_or_insert_with(Default::default);
    for (key, value) in labels_to_apply {
        deployment_labels.insert(key.clone(), value.clone());
    }

    Ok(())
}

async fn cleanup_test_resources(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
) -> std::result::Result<(), ReconcileError> {
    for test in &bgd.spec.tests {
        delete_deployment(client, namespace, &test.name).await?;
        delete_service(client, namespace, &test.name).await?;
    }

    for ip in &bgd.spec.inception_points {
        delete_deployment(client, namespace, &format!("fluidbg-{}", ip.name)).await?;
        delete_service(client, namespace, &format!("fluidbg-{}-svc", ip.name)).await?;
        delete_config_map(client, namespace, &format!("fluidbg-config-{}", ip.name)).await?;
    }

    Ok(())
}

async fn delete_deployment(
    client: &kube::Client,
    namespace: &str,
    name: &str,
) -> std::result::Result<(), ReconcileError> {
    let api: Api<Deployment> = Api::namespaced(client.clone(), namespace);
    delete_resource(api, "deployment", namespace, name).await
}

async fn delete_service(
    client: &kube::Client,
    namespace: &str,
    name: &str,
) -> std::result::Result<(), ReconcileError> {
    let api: Api<Service> = Api::namespaced(client.clone(), namespace);
    delete_resource(api, "service", namespace, name).await
}

async fn delete_config_map(
    client: &kube::Client,
    namespace: &str,
    name: &str,
) -> std::result::Result<(), ReconcileError> {
    let api: Api<ConfigMap> = Api::namespaced(client.clone(), namespace);
    delete_resource(api, "configmap", namespace, name).await
}

async fn delete_resource<K>(
    api: Api<K>,
    kind: &str,
    namespace: &str,
    name: &str,
) -> std::result::Result<(), ReconcileError>
where
    K: kube::Resource<DynamicType = ()>
        + Clone
        + serde::de::DeserializeOwned
        + std::fmt::Debug
        + Send
        + Sync
        + 'static,
{
    match api.delete(name, &DeleteParams::default()).await {
        Ok(_) => {
            info!("deleted test {} '{}/{}'", kind, namespace, name);
            Ok(())
        }
        Err(kube::Error::Api(e)) if e.code == 404 => Ok(()),
        Err(e) => Err(ReconcileError::K8s(e)),
    }
}

async fn update_status_phase(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
    phase: BGDPhase,
) {
    let name = match &bgd.metadata.name {
        Some(n) => n.clone(),
        None => return,
    };
    let api: Api<BlueGreenDeployment> = Api::namespaced(client.clone(), namespace);
    let patch = json!({ "status": { "phase": phase } });
    let pp = PatchParams::default();
    if let Err(e) = api.patch_status(&name, &pp, &Patch::Merge(&patch)).await {
        warn!("failed to update status for '{}': {}", name, e);
    }
}

async fn update_status_counts(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
    counts: &crate::state_store::Counts,
    success_rate: f64,
) {
    let name = match &bgd.metadata.name {
        Some(n) => n.clone(),
        None => return,
    };
    let api: Api<BlueGreenDeployment> = Api::namespaced(client.clone(), namespace);
    let patch = json!({
        "status": {
            "casesPassed": counts.passed,
            "casesFailed": counts.failed,
            "casesTimedOut": counts.timed_out,
            "casesPending": counts.pending,
            "casesObserved": counts.passed + counts.failed + counts.timed_out,
            "currentSuccessRate": success_rate
        }
    });
    let pp = PatchParams::default();
    if let Err(e) = api.patch_status(&name, &pp, &Patch::Merge(&patch)).await {
        warn!("failed to update status counts for '{}': {}", name, e);
    }
}
