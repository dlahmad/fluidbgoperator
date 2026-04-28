use chrono::Utc;
use kube::api::{Api, Patch, PatchParams, PostParams};
use serde_json::json;
use tracing::{info, warn};

use crate::crd::blue_green::{
    BGDPhase, BlueGreenDeployment, BlueGreenDeploymentCondition, ConditionStatus,
    InceptionPointDrainStatus,
};
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
    let mut latest = match api.get_status(&name).await {
        Ok(latest) if !phase_update_is_current(&latest, &phase, observed_generation) => {
            info!(
                "skipping stale phase update for '{}': requested {:?} generation {}",
                name, phase, observed_generation
            );
            return;
        }
        Ok(latest) => latest,
        Err(e) => {
            warn!("failed to read latest status for '{}': {}", name, e);
            return;
        }
    };

    let conditions = conditions_for_phase(&phase, observed_generation);
    let mut status = latest.status.clone().unwrap_or_default();
    status.phase = Some(phase);
    status.observed_generation = Some(observed_generation);
    status.conditions = conditions;
    latest.status = Some(status);

    match api
        .replace_status(&name, &PostParams::default(), &latest)
        .await
    {
        Ok(_) => {}
        Err(kube::Error::Api(error)) if error.code == 409 => {
            info!("status update for '{}' conflicted with a newer write", name);
        }
        Err(e) => warn!("failed to update status for '{}': {}", name, e),
    }
}

fn phase_update_is_current(
    latest: &BlueGreenDeployment,
    requested_phase: &BGDPhase,
    requested_generation: i64,
) -> bool {
    let latest_generation = latest
        .status
        .as_ref()
        .and_then(|status| status.observed_generation)
        .unwrap_or_default();
    let latest_phase = latest
        .status
        .as_ref()
        .and_then(|status| status.phase.as_ref());
    phase_transition_is_current(
        latest_phase,
        latest_generation,
        requested_phase,
        requested_generation,
    )
}

fn phase_transition_is_current(
    latest_phase: Option<&BGDPhase>,
    latest_generation: i64,
    requested_phase: &BGDPhase,
    requested_generation: i64,
) -> bool {
    if latest_generation > requested_generation {
        return false;
    }
    if latest_generation < requested_generation {
        return true;
    }

    let Some(latest_phase) = latest_phase else {
        return true;
    };
    if latest_phase == requested_phase {
        return true;
    }
    if phase_is_terminal(latest_phase) {
        return false;
    }

    phase_rank(requested_phase) >= phase_rank(latest_phase)
}

fn phase_rank(phase: &BGDPhase) -> u8 {
    match phase {
        BGDPhase::Pending => 0,
        BGDPhase::Observing => 1,
        BGDPhase::Promoting => 2,
        BGDPhase::Draining => 3,
        BGDPhase::Completed | BGDPhase::RolledBack => 4,
    }
}

fn phase_is_terminal(phase: &BGDPhase) -> bool {
    matches!(phase, BGDPhase::Completed | BGDPhase::RolledBack)
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
    let observed_generation = bgd.metadata.generation.unwrap_or_default();
    let conditions = conditions_for_phase(&BGDPhase::Pending, observed_generation);
    let patch = json!({
        "status": {
            "phase": "Pending",
            "generatedDeploymentName": null,
            "currentStep": null,
            "currentTrafficPercent": null,
            "drainStartedAt": null,
            "inceptionPointDrains": [],
            "rolloutGeneration": observed_generation,
            "observedGeneration": observed_generation,
            "conditions": conditions
        }
    });
    if let Err(e) = api
        .patch_status(&name, &PatchParams::default(), &Patch::Merge(&patch))
        .await
    {
        warn!("failed to reset rollout status for '{}': {}", name, e);
    }
}

pub(super) async fn update_status_update_deferred(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
) {
    let name = match &bgd.metadata.name {
        Some(n) => n.clone(),
        None => return,
    };
    let api: Api<BlueGreenDeployment> = Api::namespaced(client.clone(), namespace);
    let mut latest = match api.get_status(&name).await {
        Ok(latest) => latest,
        Err(e) => {
            warn!("failed to read latest status for '{}': {}", name, e);
            return;
        }
    };
    let generation = bgd.metadata.generation.unwrap_or_default();
    let rollout_generation = bgd
        .status
        .as_ref()
        .and_then(|status| status.rollout_generation)
        .unwrap_or_default();
    let now = Utc::now().to_rfc3339();
    let mut status = latest.status.clone().unwrap_or_default();
    status
        .conditions
        .retain(|condition| condition.condition_type != "UpdateDeferred");
    status.conditions.push(condition(
        "UpdateDeferred",
        ConditionStatus::True,
        "ActiveRollout",
        &format!(
            "Spec generation {generation} is deferred until rollout generation {rollout_generation} reaches a terminal phase."
        ),
        generation,
        &now,
    ));
    latest.status = Some(status);

    match api
        .replace_status(&name, &PostParams::default(), &latest)
        .await
    {
        Ok(_) => {}
        Err(kube::Error::Api(error)) if error.code == 409 => {
            info!(
                "deferred-update status for '{}' conflicted with a newer write",
                name
            );
        }
        Err(e) => warn!(
            "failed to update deferred-update status for '{}': {}",
            name, e
        ),
    }
}

pub(super) fn conditions_for_phase(
    phase: &BGDPhase,
    observed_generation: i64,
) -> Vec<BlueGreenDeploymentCondition> {
    let now = Utc::now().to_rfc3339();
    let (ready, progressing, degraded, reason, message) = match phase {
        BGDPhase::Pending => (
            ConditionStatus::False,
            ConditionStatus::True,
            ConditionStatus::False,
            "Pending",
            "Rollout is queued or initializing.",
        ),
        BGDPhase::Observing => (
            ConditionStatus::False,
            ConditionStatus::True,
            ConditionStatus::False,
            "Observing",
            "Rollout is collecting test evidence.",
        ),
        BGDPhase::Promoting => (
            ConditionStatus::False,
            ConditionStatus::True,
            ConditionStatus::False,
            "Promoting",
            "Rollout met promotion criteria and is switching traffic.",
        ),
        BGDPhase::Draining => (
            ConditionStatus::False,
            ConditionStatus::True,
            ConditionStatus::False,
            "Draining",
            "Rollout is draining temporary resources before completion.",
        ),
        BGDPhase::Completed => (
            ConditionStatus::True,
            ConditionStatus::False,
            ConditionStatus::False,
            "Completed",
            "Rollout completed successfully.",
        ),
        BGDPhase::RolledBack => (
            ConditionStatus::False,
            ConditionStatus::False,
            ConditionStatus::True,
            "RolledBack",
            "Rollout failed promotion criteria or timed out and was rolled back.",
        ),
    };

    vec![
        condition("Ready", ready, reason, message, observed_generation, &now),
        condition(
            "Progressing",
            progressing,
            reason,
            message,
            observed_generation,
            &now,
        ),
        condition(
            "Degraded",
            degraded,
            reason,
            message,
            observed_generation,
            &now,
        ),
    ]
}

fn condition(
    condition_type: &str,
    status: ConditionStatus,
    reason: &str,
    message: &str,
    observed_generation: i64,
    last_transition_time: &str,
) -> BlueGreenDeploymentCondition {
    BlueGreenDeploymentCondition {
        condition_type: condition_type.to_string(),
        status,
        reason: reason.to_string(),
        message: message.to_string(),
        observed_generation,
        last_transition_time: last_transition_time.to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn condition_status(
        conditions: &[BlueGreenDeploymentCondition],
        condition_type: &str,
    ) -> ConditionStatus {
        conditions
            .iter()
            .find(|condition| condition.condition_type == condition_type)
            .unwrap()
            .status
            .clone()
    }

    #[test]
    fn completed_conditions_are_gitops_healthy() {
        let conditions = conditions_for_phase(&BGDPhase::Completed, 7);
        assert_eq!(
            condition_status(&conditions, "Ready"),
            ConditionStatus::True
        );
        assert_eq!(
            condition_status(&conditions, "Progressing"),
            ConditionStatus::False
        );
        assert_eq!(
            condition_status(&conditions, "Degraded"),
            ConditionStatus::False
        );
        assert!(
            conditions
                .iter()
                .all(|condition| condition.observed_generation == 7)
        );
    }

    #[test]
    fn rollback_conditions_are_gitops_degraded() {
        let conditions = conditions_for_phase(&BGDPhase::RolledBack, 8);
        assert_eq!(
            condition_status(&conditions, "Ready"),
            ConditionStatus::False
        );
        assert_eq!(
            condition_status(&conditions, "Progressing"),
            ConditionStatus::False
        );
        assert_eq!(
            condition_status(&conditions, "Degraded"),
            ConditionStatus::True
        );
    }

    #[test]
    fn active_rollout_conditions_are_gitops_progressing() {
        let conditions = conditions_for_phase(&BGDPhase::Observing, 9);
        assert_eq!(
            condition_status(&conditions, "Ready"),
            ConditionStatus::False
        );
        assert_eq!(
            condition_status(&conditions, "Progressing"),
            ConditionStatus::True
        );
        assert_eq!(
            condition_status(&conditions, "Degraded"),
            ConditionStatus::False
        );
    }

    #[test]
    fn stale_phase_update_cannot_regress_terminal_status() {
        assert!(!phase_transition_is_current(
            Some(&BGDPhase::Completed),
            1,
            &BGDPhase::Observing,
            1
        ));
        assert!(!phase_transition_is_current(
            Some(&BGDPhase::RolledBack),
            1,
            &BGDPhase::Draining,
            1
        ));
    }

    #[test]
    fn phase_update_can_advance_current_generation() {
        assert!(phase_transition_is_current(
            Some(&BGDPhase::Observing),
            1,
            &BGDPhase::Promoting,
            1
        ));
        assert!(phase_transition_is_current(
            Some(&BGDPhase::Completed),
            1,
            &BGDPhase::Pending,
            2
        ));
    }
}
