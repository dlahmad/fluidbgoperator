use chrono::Utc;
use kube::api::{Api, Patch, PatchParams};
use serde_json::json;
use tracing::warn;

use crate::crd::blue_green::{BGDPhase, BlueGreenDeployment, InceptionPointDrainStatus};
use crate::state_store::Counts;

pub(super) async fn update_status_phase(
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
    let observed_generation = current_rollout_generation(bgd);
    let patch = json!({
        "status": {
            "phase": phase,
            "observedGeneration": observed_generation
        }
    });
    if let Err(e) = api
        .patch_status(&name, &PatchParams::default(), &Patch::Merge(&patch))
        .await
    {
        warn!("failed to update status for '{}': {}", name, e);
    }
}

pub(super) fn current_rollout_generation(bgd: &BlueGreenDeployment) -> i64 {
    bgd.status
        .as_ref()
        .and_then(|status| status.rollout_generation)
        .or(bgd.metadata.generation)
        .unwrap_or_default()
}

pub(super) async fn ensure_rollout_generation(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
) {
    if bgd
        .status
        .as_ref()
        .and_then(|status| status.rollout_generation)
        .is_some()
    {
        return;
    }
    let name = match &bgd.metadata.name {
        Some(n) => n.clone(),
        None => return,
    };
    let api: Api<BlueGreenDeployment> = Api::namespaced(client.clone(), namespace);
    let patch = json!({
        "status": {
            "rolloutGeneration": bgd.metadata.generation.unwrap_or_default()
        }
    });
    if let Err(e) = api
        .patch_status(&name, &PatchParams::default(), &Patch::Merge(&patch))
        .await
    {
        warn!("failed to set rollout generation for '{}': {}", name, e);
    }
}

pub(super) async fn reset_status_for_new_rollout(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
) {
    let name = match &bgd.metadata.name {
        Some(n) => n.clone(),
        None => return,
    };
    let api: Api<BlueGreenDeployment> = Api::namespaced(client.clone(), namespace);
    let patch = json!({
        "status": {
            "phase": "Pending",
            "generatedDeploymentName": null,
            "currentStep": null,
            "currentTrafficPercent": null,
            "drainStartedAt": null,
            "inceptionPointDrains": [],
            "rolloutGeneration": bgd.metadata.generation.unwrap_or_default()
        }
    });
    if let Err(e) = api
        .patch_status(&name, &PatchParams::default(), &Patch::Merge(&patch))
        .await
    {
        warn!("failed to reset rollout status for '{}': {}", name, e);
    }
}

pub(super) async fn update_drain_started_at(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
) {
    let name = match &bgd.metadata.name {
        Some(n) => n.clone(),
        None => return,
    };
    let api: Api<BlueGreenDeployment> = Api::namespaced(client.clone(), namespace);
    let patch = json!({
        "status": {
            "drainStartedAt": Utc::now().to_rfc3339()
        }
    });
    if let Err(e) = api
        .patch_status(&name, &PatchParams::default(), &Patch::Merge(&patch))
        .await
    {
        warn!("failed to update drain start for '{}': {}", name, e);
    }
}

pub(super) async fn update_inception_point_drain_statuses(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
    statuses: &[InceptionPointDrainStatus],
) {
    let name = match &bgd.metadata.name {
        Some(n) => n.clone(),
        None => return,
    };
    let api: Api<BlueGreenDeployment> = Api::namespaced(client.clone(), namespace);
    let patch = json!({
        "status": {
            "inceptionPointDrains": statuses
        }
    });
    if let Err(e) = api
        .patch_status(&name, &PatchParams::default(), &Patch::Merge(&patch))
        .await
    {
        warn!("failed to update drain statuses for '{}': {}", name, e);
    }
}

pub(super) async fn update_status_generated_deployment_name(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
    generated_deployment_name: &str,
) {
    let name = match &bgd.metadata.name {
        Some(n) => n.clone(),
        None => return,
    };
    let api: Api<BlueGreenDeployment> = Api::namespaced(client.clone(), namespace);
    let patch = json!({ "status": { "generatedDeploymentName": generated_deployment_name } });
    if let Err(e) = api
        .patch_status(&name, &PatchParams::default(), &Patch::Merge(&patch))
        .await
    {
        warn!(
            "failed to update generated deployment name for '{}': {}",
            name, e
        );
    }
}

pub(super) async fn update_status_counts(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
    counts: &Counts,
    success_rate: f64,
    last_failure_message: Option<String>,
) {
    let name = match &bgd.metadata.name {
        Some(n) => n.clone(),
        None => return,
    };
    let api: Api<BlueGreenDeployment> = Api::namespaced(client.clone(), namespace);
    let patch = json!({
        "status": {
            "testCasesPassed": counts.passed,
            "testCasesFailed": counts.failed,
            "testCasesTimedOut": counts.timed_out,
            "testCasesPending": counts.pending,
            "testCasesObserved": counts.passed + counts.failed + counts.timed_out,
            "currentSuccessRate": success_rate,
            "lastFailureMessage": last_failure_message
        }
    });
    if let Err(e) = api
        .patch_status(&name, &PatchParams::default(), &Patch::Merge(&patch))
        .await
    {
        warn!("failed to update status counts for '{}': {}", name, e);
    }
}

pub(super) async fn update_status_progress(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
    step: i32,
    traffic_percent: i32,
) {
    let name = match &bgd.metadata.name {
        Some(n) => n.clone(),
        None => return,
    };
    let api: Api<BlueGreenDeployment> = Api::namespaced(client.clone(), namespace);
    let patch = json!({
        "status": {
            "currentStep": step,
            "currentTrafficPercent": traffic_percent
        }
    });
    if let Err(e) = api
        .patch_status(&name, &PatchParams::default(), &Patch::Merge(&patch))
        .await
    {
        warn!("failed to update status progress for '{}': {}", name, e);
    }
}
