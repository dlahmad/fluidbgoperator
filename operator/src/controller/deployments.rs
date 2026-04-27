use std::collections::BTreeMap;

use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::EnvVar;
use kube::api::{Api, DeleteParams, ListParams};
use sha2::{Digest, Sha256};
use tracing::{info, warn};

use super::ReconcileError;
use super::plugin_lifecycle::{AssignmentKind, AssignmentTarget, PropertyAssignment};
use super::resources::test_instance_name;
use super::status::{current_rollout_generation, update_status_generated_deployment_name};
use crate::crd::blue_green::{
    BlueGreenDeployment, DeploymentRef, DeploymentSelector, ManagedDeploymentSpec,
};

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct DeploymentPatchKey {
    namespace: String,
    name: String,
    container_name: Option<String>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct DeploymentIdentity {
    pub(super) namespace: String,
    pub(super) name: String,
}

pub(super) async fn apply_assignments(
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
        Some(resolve_current_green(client, namespace, &bgd.spec.selector).await?)
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
                let deploy_namespace = deployment_namespace(&candidate_ref(bgd), namespace);
                let identity = DeploymentIdentity {
                    namespace: deploy_namespace.clone(),
                    name: candidate_ref(bgd).name.clone(),
                };
                let key = DeploymentPatchKey {
                    namespace: deploy_namespace.clone(),
                    name: candidate_ref(bgd).name.clone(),
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
                    let test_name = test_instance_name(bgd, &test.name);
                    let identity = DeploymentIdentity {
                        namespace: namespace.to_string(),
                        name: test_name.clone(),
                    };
                    let key = DeploymentPatchKey {
                        namespace: namespace.to_string(),
                        name: test_name,
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
        if let Some(target_name) = container_name
            && container.name != target_name
        {
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

pub(super) async fn wait_for_deployments_ready(
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

pub(super) async fn resolve_current_green(
    client: &kube::Client,
    namespace: &str,
    selector: &DeploymentSelector,
) -> std::result::Result<DeploymentRef, ReconcileError> {
    if selector.match_labels.is_empty() {
        return Err(ReconcileError::Store(
            "selector.matchLabels must contain at least one label".to_string(),
        ));
    }

    let green_namespace = selector
        .namespace
        .clone()
        .unwrap_or_else(|| namespace.to_string());
    let deploy_api: Api<Deployment> = Api::namespaced(client.clone(), &green_namespace);
    let mut labels = selector.match_labels.clone();
    labels.insert("fluidbg.io/green".to_string(), "true".to_string());
    let deployments = deploy_api
        .list(&ListParams::default().labels(&label_selector(&labels)))
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
        [] => {
            let family_deployments = deploy_api
                .list(&ListParams::default().labels(&label_selector(&selector.match_labels)))
                .await?;

            match family_deployments.items.as_slice() {
                [deployment] => {
                    let name = deployment.metadata.name.clone().ok_or_else(|| {
                        ReconcileError::Store(
                            "selected deployment for green adoption has no name".to_string(),
                        )
                    })?;
                    let mut adopted = deployment.clone();
                    set_green_label(&mut adopted, true)?;
                    deploy_api
                        .replace(&name, &Default::default(), &adopted)
                        .await?;
                    info!(
                        "adopted existing deployment '{}/{}' as current green by setting fluidbg.io/green=true",
                        green_namespace, name
                    );
                    Ok(DeploymentRef {
                        name,
                        namespace: Some(green_namespace),
                    })
                }
                [] => Err(ReconcileError::Store(format!(
                    "selector matched no deployments in namespace '{}'",
                    green_namespace
                ))),
                matches => Err(ReconcileError::Store(format!(
                    "selector matched {} deployments in namespace '{}' but none is marked fluidbg.io/green=true; refusing ambiguous adoption",
                    matches.len(),
                    green_namespace
                ))),
            }
        }
        matches => Err(ReconcileError::Store(format!(
            "selector matched {} green deployments in namespace '{}'; expected exactly one",
            matches.len(),
            green_namespace
        ))),
    }
}

pub(super) async fn delete_current_green(
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

pub(super) fn deployment_namespace(
    deployment_ref: &DeploymentRef,
    default_namespace: &str,
) -> String {
    deployment_ref
        .namespace
        .clone()
        .unwrap_or_else(|| default_namespace.to_string())
}

pub(super) fn label_selector(labels: &BTreeMap<String, String>) -> String {
    labels
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(",")
}

pub(super) fn apply_family_labels_to_deployment(
    deployment: &mut Deployment,
    labels_to_apply: &BTreeMap<String, String>,
) -> std::result::Result<(), ReconcileError> {
    if labels_to_apply.is_empty() {
        return Err(ReconcileError::Store(
            "selector.matchLabels must contain at least one label".to_string(),
        ));
    }

    let deployment_labels = deployment
        .metadata
        .labels
        .get_or_insert_with(Default::default);
    for (key, value) in labels_to_apply {
        deployment_labels.insert(key.clone(), value.clone());
    }

    let template_labels = deployment
        .spec
        .as_mut()
        .and_then(|spec| spec.template.metadata.as_mut())
        .map(|metadata| metadata.labels.get_or_insert_with(Default::default))
        .ok_or_else(|| ReconcileError::Store("deployment template metadata missing".to_string()))?;
    for (key, value) in labels_to_apply {
        template_labels.insert(key.clone(), value.clone());
    }

    Ok(())
}

pub(super) fn apply_rollout_candidate_labels(
    deployment: &mut Deployment,
    bgd: &BlueGreenDeployment,
) -> std::result::Result<(), ReconcileError> {
    let bgd_name = bgd.metadata.name.as_deref().unwrap_or("");
    let bgd_uid = bgd.metadata.uid.as_deref().unwrap_or("");
    let deployment_labels = deployment
        .metadata
        .labels
        .get_or_insert_with(Default::default);
    deployment_labels.insert(
        "fluidbg.io/blue-green-ref".to_string(),
        bgd_name.to_string(),
    );
    deployment_labels.insert("fluidbg.io/blue-green-uid".to_string(), bgd_uid.to_string());

    let template_labels = deployment
        .spec
        .as_mut()
        .and_then(|spec| spec.template.metadata.as_mut())
        .map(|metadata| metadata.labels.get_or_insert_with(Default::default))
        .ok_or_else(|| ReconcileError::Store("deployment template metadata missing".to_string()))?;
    template_labels.insert(
        "fluidbg.io/blue-green-ref".to_string(),
        bgd_name.to_string(),
    );
    template_labels.insert("fluidbg.io/blue-green-uid".to_string(), bgd_uid.to_string());
    Ok(())
}

pub(super) fn clear_rollout_candidate_labels(
    deployment: &mut Deployment,
) -> std::result::Result<(), ReconcileError> {
    if let Some(labels) = deployment.metadata.labels.as_mut() {
        labels.remove("fluidbg.io/blue-green-ref");
        labels.remove("fluidbg.io/blue-green-uid");
    }
    let template_labels = deployment
        .spec
        .as_mut()
        .and_then(|spec| spec.template.metadata.as_mut())
        .map(|metadata| metadata.labels.get_or_insert_with(Default::default))
        .ok_or_else(|| ReconcileError::Store("deployment template metadata missing".to_string()))?;
    template_labels.remove("fluidbg.io/blue-green-ref");
    template_labels.remove("fluidbg.io/blue-green-uid");
    Ok(())
}

pub(super) fn set_green_label(
    deployment: &mut Deployment,
    is_green: bool,
) -> std::result::Result<(), ReconcileError> {
    let value = if is_green { "true" } else { "false" }.to_string();
    deployment
        .metadata
        .labels
        .get_or_insert_with(Default::default)
        .insert("fluidbg.io/green".to_string(), value.clone());
    let template_labels = deployment
        .spec
        .as_mut()
        .and_then(|spec| spec.template.metadata.as_mut())
        .map(|metadata| metadata.labels.get_or_insert_with(Default::default))
        .ok_or_else(|| ReconcileError::Store("deployment template metadata missing".to_string()))?;
    template_labels.insert("fluidbg.io/green".to_string(), value);
    Ok(())
}

pub(super) fn candidate_ref(bgd: &BlueGreenDeployment) -> DeploymentRef {
    DeploymentRef {
        name: bgd
            .status
            .as_ref()
            .and_then(|status| status.generated_deployment_name.clone())
            .unwrap_or_else(|| {
                let bgd_name = bgd.metadata.name.as_deref().unwrap_or("bgd");
                format!("{bgd_name}-pending")
            }),
        namespace: bgd.spec.deployment.namespace.clone(),
    }
}

pub(super) fn generated_candidate_name_seed(bgd: &BlueGreenDeployment) -> String {
    let bgd_name = bgd.metadata.name.as_deref().unwrap_or("bgd");
    let uid = bgd.metadata.uid.as_deref().unwrap_or("");
    let generation = current_rollout_generation(bgd);
    format!("{bgd_name}:{uid}:{generation}")
}

pub(super) fn candidate_name_with_suffix(base: &str, suffix: &str) -> String {
    let max_base_len = 253usize.saturating_sub(suffix.len() + 1);
    let trimmed_base = if base.len() > max_base_len {
        &base[..max_base_len]
    } else {
        base
    };
    format!("{trimmed_base}-{suffix}")
}

pub(super) fn deterministic_candidate_suffixes(seed: &str) -> Vec<String> {
    (0..32)
        .map(|attempt| {
            let digest = Sha256::digest(format!("{seed}:{attempt}").as_bytes());
            let mut suffix = String::with_capacity(6);
            for byte in digest.iter() {
                let ch = match byte % 36 {
                    value @ 0..=9 => (b'0' + value) as char,
                    value => (b'a' + (value - 10)) as char,
                };
                suffix.push(ch);
                if suffix.len() == 6 {
                    break;
                }
            }
            suffix
        })
        .collect()
}

pub(super) async fn ensure_generated_candidate_name(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
) -> std::result::Result<Option<String>, ReconcileError> {
    if let Some(existing) = bgd
        .status
        .as_ref()
        .and_then(|status| status.generated_deployment_name.clone())
    {
        return Ok(Some(existing));
    }

    let deploy_namespace = deployment_namespace_spec(&bgd.spec.deployment, namespace);
    let deploy_api: Api<Deployment> = Api::namespaced(client.clone(), &deploy_namespace);
    let bgd_name = bgd.metadata.name.as_deref().unwrap_or("bgd");
    let seed = generated_candidate_name_seed(bgd);

    for candidate_suffix in deterministic_candidate_suffixes(&seed) {
        let candidate_name = candidate_name_with_suffix(bgd_name, &candidate_suffix);

        match deploy_api.get(&candidate_name).await {
            Ok(_) => continue,
            Err(kube::Error::Api(err)) if err.code == 404 => {
                update_status_generated_deployment_name(bgd, client, namespace, &candidate_name)
                    .await;
                return Ok(None);
            }
            Err(err) => return Err(ReconcileError::K8s(err)),
        }
    }

    Err(ReconcileError::Store(
        "failed to generate a non-colliding deployment name after 32 attempts".to_string(),
    ))
}

pub(super) fn deployment_namespace_spec(
    deployment_spec: &ManagedDeploymentSpec,
    default_namespace: &str,
) -> String {
    deployment_spec
        .namespace
        .clone()
        .unwrap_or_else(|| default_namespace.to_string())
}
