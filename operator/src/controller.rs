use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use kube::api::{Api, Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::runtime::{Controller, watcher};
use serde_json::json;
use thiserror::Error;
use tracing::{error, info};

use crate::crd::blue_green::{BGDPhase, BlueGreenDeployment};
use crate::state_store::StateStore;
use crate::state_store::VerificationMode;
use crate::strategy::PromotionAction;

mod deployments;
mod phases;
mod plugin_lifecycle;
mod promotion;
mod resources;
mod status;

#[cfg(test)]
mod tests;

#[cfg(test)]
use crate::crd::blue_green::ManagedDeploymentSpec;
use deployments::{
    apply_assignments, apply_family_labels_to_deployment, apply_rollout_candidate_labels,
    candidate_ref, clear_rollout_candidate_labels, deployment_namespace_spec,
    resolve_current_green, set_green_label, wait_for_deployments_ready,
};
#[cfg(test)]
use deployments::{
    candidate_name_with_suffix, deterministic_candidate_suffixes, generated_candidate_name_seed,
};
#[cfg(test)]
use phases::select_previous_green_for_promotion;
#[cfg(test)]
use phases::validate_progressive_splitter_plugin;

use deployments::ensure_generated_candidate_name;
use phases::{
    apply_splitter_traffic_percent, begin_draining_after_promotion, begin_draining_after_rollback,
    bootstrap_initial_green_if_empty, ensure_declared_deployments, ensure_inception_resources,
    initialize_splitter_traffic, promote, reconcile_draining,
    validate_progressive_shifting_support,
};
use promotion::{decide_promotion_action, validate_test_configuration};
use resources::{
    blue_green_refs_from_owned_resources, cleanup_inception_resources,
    cleanup_orphaned_blue_green_resources, cleanup_test_resources, ensure_test_resources,
};
use status::{
    ensure_rollout_generation, reset_status_for_new_rollout, update_drain_started_at,
    update_status_counts, update_status_phase, update_status_progress,
};

const BGD_FINALIZER: &str = "fluidbg.io/cleanup";

struct Ctx {
    client: kube::Client,
    namespace: String,
    auth: AuthConfig,
    store: Arc<dyn StateStore>,
}

#[derive(Clone)]
pub struct AuthConfig {
    pub signing_secret_namespace: String,
    pub signing_secret_name: String,
    pub signing_secret_key: String,
}

#[derive(Debug, Error)]
pub(super) enum ReconcileError {
    #[error("store error: {0}")]
    Store(String),
    #[error("k8s error: {0}")]
    K8s(#[from] kube::Error),
}

pub async fn run_controller(
    client: kube::Client,
    namespace: String,
    auth: AuthConfig,
    store: Arc<dyn StateStore>,
) {
    let bgd_api: Api<BlueGreenDeployment> = Api::namespaced(client.clone(), &namespace);

    let ctx = Arc::new(Ctx {
        client,
        namespace,
        auth,
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

pub async fn run_orphan_cleanup(
    client: kube::Client,
    namespace: String,
    store: Arc<dyn StateStore>,
    interval: Duration,
) {
    let mut tick = tokio::time::interval(interval);
    loop {
        tick.tick().await;
        if let Err(err) = cleanup_orphaned_blue_green_refs(&client, &namespace, &store).await {
            error!("orphan cleanup failed: {}", err);
        }
    }
}

pub(crate) async fn validate_plugin_auth(
    client: &kube::Client,
    namespace: &str,
    auth: &AuthConfig,
    header_value: Option<&str>,
) -> std::result::Result<Option<fluidbg_plugin_sdk::PluginAuthClaims>, String> {
    resources::validate_inception_auth_token(client, namespace, auth, header_value)
        .await
        .map_err(|err| err.to_string())
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

    if bgd.metadata.deletion_timestamp.is_some() {
        cleanup_deleted_bgd(&bgd, &ctx, &name, &namespace).await?;
        return Ok(Action::await_change());
    }

    if !has_finalizer(&bgd, BGD_FINALIZER) {
        ensure_finalizer(&bgd, &client, &namespace).await?;
        return Ok(Action::requeue(std::time::Duration::from_secs(1)));
    }

    if rollout_needs_restart(&bgd, &phase) {
        info!(
            "detected new spec generation for terminal BGD '{}'; cleaning previous rollout before restart",
            name
        );
        cleanup_inception_resources(&bgd, &client, &namespace).await?;
        cleanup_test_resources(&bgd, &client, &namespace).await?;
        reset_status_for_new_rollout(&bgd, &client, &namespace).await;
        return Ok(Action::requeue(std::time::Duration::from_secs(1)));
    }

    info!(
        "reconciling BlueGreenDeployment '{}' phase={:?}",
        name, phase
    );

    match phase {
        BGDPhase::Pending => {
            ensure_rollout_generation(&bgd, &client, &namespace).await;
            let Some(generated_name) =
                ensure_generated_candidate_name(&bgd, &client, &namespace).await?
            else {
                return Ok(Action::requeue(std::time::Duration::from_secs(1)));
            };
            info!("using generated deployment name '{}'", generated_name);
            if bootstrap_initial_green_if_empty(&bgd, &client, &namespace).await? {
                update_status_phase(&bgd, &client, &namespace, BGDPhase::Completed).await;
                return Ok(Action::requeue(std::time::Duration::from_secs(300)));
            }
            resolve_current_green(&client, &namespace, &bgd.spec.selector).await?;
            validate_progressive_shifting_support(&bgd, &client, &namespace).await?;
            ensure_declared_deployments(&bgd, &client, &namespace).await?;
            ensure_test_resources(&bgd, &client, &namespace).await?;
            ensure_inception_resources(&bgd, &client, &namespace, &ctx.auth).await?;
            initialize_splitter_traffic(&bgd, &client, &namespace, &ctx.auth).await?;
            update_status_phase(&bgd, &client, &namespace, BGDPhase::Observing).await;
            Ok(Action::requeue(std::time::Duration::from_secs(5)))
        }
        BGDPhase::Observing => {
            validate_test_configuration(&bgd)?;

            let counts = ctx
                .store
                .counts(&name)
                .await
                .map_err(|e| ReconcileError::Store(e.to_string()))?;
            let data_counts = ctx
                .store
                .counts_for_mode(&name, VerificationMode::Data)
                .await
                .map_err(|e| ReconcileError::Store(e.to_string()))?;
            let custom_counts = ctx
                .store
                .counts_for_mode(&name, VerificationMode::Custom)
                .await
                .map_err(|e| ReconcileError::Store(e.to_string()))?;
            let latest_failure_message = ctx
                .store
                .latest_failure_message(&name)
                .await
                .map_err(|e| ReconcileError::Store(e.to_string()))?;

            let total = counts.passed + counts.failed + counts.timed_out;
            let success_rate = if total > 0 {
                counts.passed as f64 / total as f64
            } else {
                0.0
            };

            update_status_counts(
                &bgd,
                &client,
                &namespace,
                &counts,
                success_rate,
                latest_failure_message,
            )
            .await;

            let action = decide_promotion_action(
                &bgd,
                &data_counts,
                &custom_counts,
                bgd.status.as_ref().and_then(|s| s.current_step),
            )
            .await?;

            match action {
                PromotionAction::ContinueObserving => {
                    info!(
                        "BGD '{}' observing: total={}, rate={:.4}",
                        name, total, success_rate
                    );
                }
                PromotionAction::Promote => {
                    info!(
                        "BGD '{}' promotion threshold met! rate={:.4}",
                        name, success_rate
                    );
                    update_status_phase(&bgd, &client, &namespace, BGDPhase::Promoting).await;
                }
                PromotionAction::Rollback => {
                    info!("BGD '{}' rolling back: rate={:.4}", name, success_rate);
                    begin_draining_after_rollback(&bgd, &client, &namespace, &ctx.auth).await?;
                    update_status_phase(&bgd, &client, &namespace, BGDPhase::Draining).await;
                    update_drain_started_at(&bgd, &client, &namespace).await;
                }
                PromotionAction::AdvanceStep {
                    step,
                    traffic_percent,
                } => {
                    apply_splitter_traffic_percent(
                        &bgd,
                        &client,
                        &namespace,
                        &ctx.auth,
                        traffic_percent,
                    )
                    .await?;
                    update_status_progress(&bgd, &client, &namespace, step, traffic_percent).await;
                    info!(
                        "BGD '{}' advanced progressive splitter traffic to {}% (step {})",
                        name, traffic_percent, step
                    );
                }
            }
            Ok(Action::requeue(std::time::Duration::from_secs(5)))
        }
        BGDPhase::Promoting => {
            let previous_green = promote(&bgd, &client, &namespace).await?;
            begin_draining_after_promotion(&bgd, &client, &namespace, &ctx.auth, &previous_green)
                .await?;
            update_status_phase(&bgd, &client, &namespace, BGDPhase::Draining).await;
            update_drain_started_at(&bgd, &client, &namespace).await;
            Ok(Action::requeue(std::time::Duration::from_secs(5)))
        }
        BGDPhase::Draining => {
            reconcile_draining(&bgd, &client, &namespace, &ctx.auth).await?;
            Ok(Action::requeue(std::time::Duration::from_secs(5)))
        }
        BGDPhase::Completed | BGDPhase::RolledBack => {
            info!("BGD '{}' in terminal state {:?}", name, phase);
            cleanup_inception_resources(&bgd, &client, &namespace).await?;
            cleanup_test_resources(&bgd, &client, &namespace).await?;
            let removed = ctx
                .store
                .cleanup_blue_green(&name)
                .await
                .map_err(|e| ReconcileError::Store(e.to_string()))?;
            if removed > 0 {
                info!("cleaned {} store records for BGD '{}'", removed, name);
            }
            Ok(Action::requeue(std::time::Duration::from_secs(300)))
        }
    }
}

async fn cleanup_deleted_bgd(
    bgd: &BlueGreenDeployment,
    ctx: &Arc<Ctx>,
    name: &str,
    namespace: &str,
) -> std::result::Result<(), ReconcileError> {
    info!("cleaning deleted BlueGreenDeployment '{}'", name);
    cleanup_inception_resources(bgd, &ctx.client, namespace).await?;
    cleanup_test_resources(bgd, &ctx.client, namespace).await?;
    let removed = ctx
        .store
        .cleanup_blue_green(name)
        .await
        .map_err(|e| ReconcileError::Store(e.to_string()))?;
    if removed > 0 {
        info!(
            "cleaned {} store records for deleted BGD '{}'",
            removed, name
        );
    }
    remove_finalizer(bgd, &ctx.client, namespace).await?;
    Ok(())
}

async fn cleanup_orphaned_blue_green_refs(
    client: &kube::Client,
    namespace: &str,
    store: &Arc<dyn StateStore>,
) -> std::result::Result<usize, ReconcileError> {
    let existing = existing_blue_green_names(client, namespace).await?;
    let mut refs = store
        .list_blue_green_refs()
        .await
        .map_err(|e| ReconcileError::Store(e.to_string()))?;
    refs.extend(blue_green_refs_from_owned_resources(client, namespace).await?);

    let mut cleaned = 0;
    for blue_green_ref in refs {
        if existing.contains(&blue_green_ref) {
            continue;
        }
        info!(
            "cleaning orphaned resources and store records for missing BGD '{}'",
            blue_green_ref
        );
        cleanup_orphaned_blue_green_resources(client, namespace, &blue_green_ref).await?;
        let removed = store
            .cleanup_blue_green(&blue_green_ref)
            .await
            .map_err(|e| ReconcileError::Store(e.to_string()))?;
        if removed > 0 {
            info!(
                "cleaned {} store records for orphaned BGD '{}'",
                removed, blue_green_ref
            );
        }
        cleaned += 1;
    }
    Ok(cleaned)
}

async fn existing_blue_green_names(
    client: &kube::Client,
    namespace: &str,
) -> std::result::Result<BTreeSet<String>, ReconcileError> {
    let api: Api<BlueGreenDeployment> = Api::namespaced(client.clone(), namespace);
    Ok(api
        .list(&Default::default())
        .await?
        .items
        .into_iter()
        .filter_map(|bgd| bgd.metadata.name)
        .collect())
}

fn has_finalizer(bgd: &BlueGreenDeployment, finalizer: &str) -> bool {
    bgd.metadata
        .finalizers
        .as_deref()
        .unwrap_or_default()
        .iter()
        .any(|value| value == finalizer)
}

async fn ensure_finalizer(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
) -> std::result::Result<(), ReconcileError> {
    let Some(name) = bgd.metadata.name.as_deref() else {
        return Ok(());
    };
    let api: Api<BlueGreenDeployment> = Api::namespaced(client.clone(), namespace);
    let patch = json!({
        "metadata": {
            "finalizers": [BGD_FINALIZER]
        }
    });
    api.patch(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await?;
    Ok(())
}

async fn remove_finalizer(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
) -> std::result::Result<(), ReconcileError> {
    let Some(name) = bgd.metadata.name.as_deref() else {
        return Ok(());
    };
    let finalizers = bgd
        .metadata
        .finalizers
        .clone()
        .unwrap_or_default()
        .into_iter()
        .filter(|value| value != BGD_FINALIZER)
        .collect::<Vec<_>>();
    let patch = if finalizers.is_empty() {
        json!({ "metadata": { "finalizers": null } })
    } else {
        json!({ "metadata": { "finalizers": finalizers } })
    };
    let api: Api<BlueGreenDeployment> = Api::namespaced(client.clone(), namespace);
    api.patch(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await?;
    Ok(())
}

fn rollout_needs_restart(bgd: &BlueGreenDeployment, phase: &BGDPhase) -> bool {
    if !matches!(*phase, BGDPhase::Completed | BGDPhase::RolledBack) {
        return false;
    }
    let generation = bgd.metadata.generation.unwrap_or_default();
    let observed_generation = bgd
        .status
        .as_ref()
        .and_then(|status| status.observed_generation)
        .unwrap_or_default();
    generation > observed_generation
}
