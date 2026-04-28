use k8s_openapi::api::apps::v1::Deployment;
use kube::api::{Api, ListParams};
use tracing::info;

use super::super::deployments::{
    candidate_ref, delete_current_green, deployment_namespace, deployment_namespace_spec,
    label_selector, wait_for_deployments_ready,
};
use super::super::plugin_lifecycle::PropertyAssignment;
use super::super::plugin_lifecycle::{AssignmentTarget, start_plugin_draining};
use super::super::resources::{apply_deployment_manifest, delete_deployment};
use super::super::{AuthConfig, ReconcileError};
use crate::crd::blue_green::{BlueGreenDeployment, DeploymentRef, DeploymentSelector};

pub(in crate::controller) async fn promote(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
) -> std::result::Result<DeploymentRef, ReconcileError> {
    let candidate = candidate_ref(bgd);
    let current_green =
        resolve_previous_green_for_promotion(client, namespace, &bgd.spec.selector, &candidate)
            .await?;
    let candidate_identity = apply_deployment_manifest(
        client,
        bgd,
        namespace,
        &bgd.spec.deployment,
        &bgd.spec.selector.match_labels,
        true,
        &[],
    )
    .await?;
    wait_for_deployments_ready(client, &[candidate_identity]).await?;

    info!(
        "promoted: deployment '{}/{}' now marked fluidbg.io/green=true",
        deployment_namespace(&candidate, namespace),
        candidate.name
    );

    Ok(current_green)
}

async fn resolve_previous_green_for_promotion(
    client: &kube::Client,
    namespace: &str,
    selector: &DeploymentSelector,
    candidate: &DeploymentRef,
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

    select_previous_green_for_promotion(&green_namespace, &deployments.items, candidate, namespace)
}

pub(in crate::controller) fn select_previous_green_for_promotion(
    green_namespace: &str,
    green_deployments: &[Deployment],
    candidate: &DeploymentRef,
    default_namespace: &str,
) -> std::result::Result<DeploymentRef, ReconcileError> {
    let candidate_namespace = deployment_namespace(candidate, default_namespace);
    let mut candidate_seen = false;
    let mut previous = Vec::new();

    for deployment in green_deployments {
        let name = deployment.metadata.name.clone().ok_or_else(|| {
            ReconcileError::Store("selected green deployment has no name".to_string())
        })?;
        if name == candidate.name && green_namespace == candidate_namespace {
            candidate_seen = true;
        } else {
            previous.push(DeploymentRef {
                name,
                namespace: Some(green_namespace.to_string()),
            });
        }
    }

    match (candidate_seen, previous.as_slice()) {
        (true, [deployment]) => Ok(deployment.clone()),
        (true, []) => Ok(candidate.clone()),
        (true, deployments) => Err(ReconcileError::Store(format!(
            "promotion found candidate plus {} previous green deployments in namespace '{}'; expected at most one previous green",
            deployments.len(),
            green_namespace
        ))),
        (false, [deployment]) => Ok(deployment.clone()),
        (false, []) => Err(ReconcileError::Store(format!(
            "selector matched no green deployments in namespace '{}'",
            green_namespace
        ))),
        (false, deployments) => Err(ReconcileError::Store(format!(
            "selector matched {} green deployments in namespace '{}'; expected exactly one or an in-progress promotion candidate plus previous green",
            deployments.len(),
            green_namespace
        ))),
    }
}

pub(in crate::controller) async fn ensure_declared_deployments(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
    initial_assignments: &[PropertyAssignment],
) -> std::result::Result<(), ReconcileError> {
    apply_deployment_manifest(
        client,
        bgd,
        namespace,
        &bgd.spec.deployment,
        &bgd.spec.selector.match_labels,
        false,
        initial_assignments,
    )
    .await?;
    Ok(())
}

pub(in crate::controller) async fn bootstrap_initial_green_if_empty(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
) -> std::result::Result<bool, ReconcileError> {
    if bgd.spec.selector.match_labels.is_empty() {
        return Err(ReconcileError::Store(
            "selector.matchLabels must contain at least one label".to_string(),
        ));
    }

    let green_namespace = bgd
        .spec
        .selector
        .namespace
        .clone()
        .unwrap_or_else(|| namespace.to_string());
    let deploy_api: Api<Deployment> = Api::namespaced(client.clone(), &green_namespace);
    let family = deploy_api
        .list(&ListParams::default().labels(&label_selector(&bgd.spec.selector.match_labels)))
        .await?;

    if !family.items.is_empty() {
        return Ok(false);
    }

    apply_deployment_manifest(
        client,
        bgd,
        namespace,
        &bgd.spec.deployment,
        &bgd.spec.selector.match_labels,
        true,
        &[],
    )
    .await?;

    info!(
        "bootstrapped initial green deployment '{}/{}' from spec.deployment because selector matched no existing deployments",
        deployment_namespace_spec(&bgd.spec.deployment, namespace),
        candidate_ref(bgd).name
    );
    Ok(true)
}

pub(in crate::controller) async fn begin_draining_after_promotion(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
    auth: &AuthConfig,
    previous_green: &DeploymentRef,
) -> std::result::Result<(), ReconcileError> {
    start_plugin_draining(bgd, client, namespace, auth, &[AssignmentTarget::Blue]).await?;
    delete_current_green(client, namespace, &candidate_ref(bgd), previous_green).await?;
    Ok(())
}

pub(in crate::controller) async fn begin_draining_after_rollback(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
    auth: &AuthConfig,
) -> std::result::Result<(), ReconcileError> {
    start_plugin_draining(bgd, client, namespace, auth, &[AssignmentTarget::Green]).await?;
    delete_deployment(
        client,
        &deployment_namespace(&candidate_ref(bgd), namespace),
        &candidate_ref(bgd).name,
    )
    .await?;
    Ok(())
}
