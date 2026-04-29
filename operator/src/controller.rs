use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use k8s_openapi::api::core::v1::ConfigMap;
use kube::ResourceExt;
use kube::api::{Api, Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::runtime::{Controller, watcher};
use serde_json::json;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracing::{error, info, warn};

use crate::crd::blue_green::{
    ActiveRolloutUpdatePolicy, BGDPhase, BlueGreenDeployment, BlueGreenDeploymentSpec,
};
use crate::crd::inception_plugin::InceptionPlugin;
use crate::state_store::StateStore;
use crate::state_store::VerificationMode;
use crate::strategy::PromotionAction;

mod deployments;
mod lease;
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
    apply_family_labels_to_deployment, apply_rollout_candidate_labels, candidate_ref,
    clear_rollout_candidate_labels, deployment_namespace_spec, resolve_current_green,
    set_green_label,
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
use lease::{LeaseConfig, run_with_bgd_lease, run_with_orphan_lease};
use phases::{
    apply_splitter_traffic_percent, begin_draining_after_promotion, begin_draining_after_rollback,
    bootstrap_initial_green_if_empty, ensure_inception_resources, initialize_splitter_traffic,
    promote, reconcile_draining, validate_progressive_shifting_support,
};
use promotion::{decide_promotion_action, validate_test_configuration};
use resources::{
    cleanup_inception_resources, cleanup_orphaned_blue_green_resources, cleanup_test_resources,
};
use status::{
    ensure_rollout_generation, reset_status_for_new_rollout, update_drain_started_at,
    update_status_counts, update_status_phase, update_status_progress,
    update_status_update_deferred,
};

const BGD_FINALIZER: &str = "fluidbg.io/cleanup";

struct Ctx {
    client: kube::Client,
    auth: AuthConfig,
    store: Arc<dyn StateStore>,
    lease: LeaseConfig,
    local_locks: Arc<Mutex<BTreeSet<String>>>,
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

pub async fn run_controller(client: kube::Client, auth: AuthConfig, store: Arc<dyn StateStore>) {
    let bgd_api: Api<BlueGreenDeployment> = Api::all(client.clone());

    let ctx = Arc::new(Ctx {
        client,
        auth,
        store,
        lease: lease_config_from_env(),
        local_locks: Arc::new(Mutex::new(BTreeSet::new())),
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
    auth: AuthConfig,
    store: Arc<dyn StateStore>,
    interval: Duration,
) {
    let mut tick = tokio::time::interval(interval);
    loop {
        tick.tick().await;
        let lease = lease_config_from_env();
        if let Err(err) = cleanup_orphaned_blue_green_refs(&client, &store, &lease).await {
            error!("orphan cleanup failed: {}", err);
        }
        if let Err(err) = sync_plugin_managers(&client, &auth).await {
            error!("plugin manager sync failed: {}", err);
        }
    }
}

fn operator_identity() -> String {
    std::env::var("FLUIDBG_OPERATOR_ID")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| format!("fluidbg-operator-{}", std::process::id()))
}

fn lease_config_from_env() -> LeaseConfig {
    let duration_seconds = env_u64("FLUIDBG_RECONCILE_LEASE_DURATION_SECONDS", 30);
    let mut renew_seconds = env_u64("FLUIDBG_RECONCILE_LEASE_RENEW_INTERVAL_SECONDS", 10);
    if renew_seconds >= duration_seconds {
        renew_seconds = (duration_seconds / 3).max(1);
    }
    LeaseConfig {
        holder_identity: operator_identity(),
        duration: Duration::from_secs(duration_seconds),
        renew_interval: Duration::from_secs(renew_seconds),
    }
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

pub(crate) async fn validate_plugin_auth(
    client: &kube::Client,
    auth: &AuthConfig,
    header_value: Option<&str>,
) -> std::result::Result<Option<fluidbg_plugin_sdk::PluginAuthClaims>, String> {
    resources::validate_inception_auth_token(client, auth, header_value)
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
    let Some(namespace) = bgd.namespace() else {
        return Ok(Action::await_change());
    };
    let reconcile_namespace = namespace.clone();
    let client = ctx.client.clone();
    let name = bgd.metadata.name.clone();
    let lease = ctx.lease.clone();
    let Some(_local_lock) = try_acquire_local_reconcile_lock(&bgd, &ctx) else {
        return Ok(Action::requeue(std::time::Duration::from_secs(1)));
    };
    if bgd.metadata.deletion_timestamp.is_some() {
        let Some(name) = name else {
            return Ok(Action::await_change());
        };
        let api: Api<BlueGreenDeployment> = Api::namespaced(client, &reconcile_namespace);
        let latest = match api.get(&name).await {
            Ok(latest) => latest,
            Err(kube::Error::Api(error)) if error.code == 404 => {
                return Ok(Action::await_change());
            }
            Err(error) => return Err(ReconcileError::K8s(error)),
        };
        cleanup_deleted_bgd(Arc::new(latest), ctx).await?;
        return Ok(Action::await_change());
    }
    match run_with_bgd_lease(
        bgd.as_ref(),
        client.clone(),
        &namespace,
        &lease,
        async move {
            let Some(name) = name else {
                return Ok(Action::await_change());
            };
            let api: Api<BlueGreenDeployment> = Api::namespaced(client, &reconcile_namespace);
            let latest = match api.get(&name).await {
                Ok(latest) => latest,
                Err(kube::Error::Api(error)) if error.code == 404 => {
                    return Ok(Action::await_change());
                }
                Err(error) => return Err(ReconcileError::K8s(error)),
            };
            reconcile_locked(Arc::new(latest), ctx).await
        },
    )
    .await?
    {
        Some(action) => Ok(action),
        None => Ok(Action::requeue(std::time::Duration::from_secs(2))),
    }
}

struct LocalReconcileLock {
    key: String,
    locks: Arc<Mutex<BTreeSet<String>>>,
}

impl Drop for LocalReconcileLock {
    fn drop(&mut self) {
        if let Ok(mut locks) = self.locks.lock() {
            locks.remove(&self.key);
        }
    }
}

fn try_acquire_local_reconcile_lock(
    bgd: &BlueGreenDeployment,
    ctx: &Arc<Ctx>,
) -> Option<LocalReconcileLock> {
    let namespace = bgd.namespace().unwrap_or_else(|| "unknown".to_string());
    let name = bgd
        .metadata
        .name
        .as_deref()
        .unwrap_or("unknown")
        .to_string();
    let key = bgd
        .metadata
        .uid
        .as_deref()
        .map(|uid| format!("{namespace}/{uid}"))
        .unwrap_or_else(|| blue_green_state_key(&namespace, &name));
    let mut locks = ctx.local_locks.lock().ok()?;
    if !locks.insert(key.clone()) {
        return None;
    }
    Some(LocalReconcileLock {
        key,
        locks: ctx.local_locks.clone(),
    })
}

async fn reconcile_locked(
    bgd: Arc<BlueGreenDeployment>,
    ctx: Arc<Ctx>,
) -> std::result::Result<Action, ReconcileError> {
    let name = bgd
        .metadata
        .name
        .as_deref()
        .unwrap_or("unknown")
        .to_string();
    let namespace = bgd.namespace().ok_or_else(|| {
        ReconcileError::Store(format!(
            "BlueGreenDeployment '{}' is missing metadata.namespace",
            name
        ))
    })?;
    let client = ctx.client.clone();
    let state_key = blue_green_state_key(&namespace, &name);

    let phase = bgd
        .status
        .as_ref()
        .and_then(|s| s.phase.clone())
        .unwrap_or(BGDPhase::Pending);

    if bgd.metadata.deletion_timestamp.is_some() {
        cleanup_deleted_bgd(bgd, ctx).await?;
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
        delete_rollout_spec_snapshot(&bgd, &client, &namespace).await?;
        let removed = ctx
            .store
            .cleanup_blue_green(&state_key)
            .await
            .map_err(|e| ReconcileError::Store(e.to_string()))?;
        if removed > 0 {
            info!(
                "cleaned {} store records for restarted BGD '{}'",
                removed, name
            );
        }
        reset_status_for_new_rollout(&bgd, &client, &namespace).await;
        return Ok(Action::requeue(std::time::Duration::from_secs(1)));
    }

    let bgd = if active_rollout_has_new_spec_generation(&bgd, &phase) {
        if should_start_force_replace_rollout(&bgd, &phase) {
            warn!(
                "force-replacing active rollout for BGD '{}' generation {} over rollout generation {}; starting rollback-style drain before new rollout",
                name,
                bgd.metadata.generation.unwrap_or_default(),
                current_rollout_generation_from_bgd(&bgd)
            );
            start_force_replace_rollout(&bgd, &client, &namespace, &ctx.auth).await?;
            return Ok(Action::requeue(std::time::Duration::from_secs(1)));
        } else {
            if !force_replace_active_rollout(&bgd) {
                update_status_update_deferred(&bgd, &client, &namespace).await;
            }
            Arc::new(load_rollout_spec_snapshot(&bgd, &client, &namespace).await?)
        }
    } else {
        bgd
    };

    info!(
        "reconciling BlueGreenDeployment '{}' phase={:?}",
        name, phase
    );

    match phase {
        BGDPhase::Pending => {
            ensure_rollout_generation(&bgd, &client, &namespace).await;
            ensure_rollout_spec_snapshot(&bgd, &client, &namespace).await?;
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
            ensure_inception_resources(&bgd, &client, &namespace, &ctx.auth).await?;
            initialize_splitter_traffic(&bgd, &client, &namespace, &ctx.auth).await?;
            update_status_phase(&bgd, &client, &namespace, BGDPhase::Observing).await;
            Ok(Action::requeue(std::time::Duration::from_secs(5)))
        }
        BGDPhase::Observing => {
            validate_test_configuration(&bgd)?;

            let counts = ctx
                .store
                .counts(&state_key)
                .await
                .map_err(|e| ReconcileError::Store(e.to_string()))?;
            let data_counts = ctx
                .store
                .counts_for_mode(&state_key, VerificationMode::Data)
                .await
                .map_err(|e| ReconcileError::Store(e.to_string()))?;
            let custom_counts = ctx
                .store
                .counts_for_mode(&state_key, VerificationMode::Custom)
                .await
                .map_err(|e| ReconcileError::Store(e.to_string()))?;
            let latest_failure_message = ctx
                .store
                .latest_failure_message(&state_key)
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
            reconcile_draining(&bgd, &client, &namespace, &ctx.auth, &ctx.store, &state_key)
                .await?;
            Ok(Action::requeue(std::time::Duration::from_secs(5)))
        }
        BGDPhase::Completed | BGDPhase::RolledBack => {
            info!("BGD '{}' in terminal state {:?}", name, phase);
            cleanup_inception_resources(&bgd, &client, &namespace).await?;
            cleanup_test_resources(&bgd, &client, &namespace).await?;
            delete_rollout_spec_snapshot(&bgd, &client, &namespace).await?;
            let removed = ctx
                .store
                .cleanup_blue_green(&state_key)
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
    bgd: Arc<BlueGreenDeployment>,
    ctx: Arc<Ctx>,
) -> std::result::Result<(), ReconcileError> {
    let name = bgd
        .metadata
        .name
        .as_deref()
        .unwrap_or("unknown")
        .to_string();
    let namespace = bgd.namespace().ok_or_else(|| {
        ReconcileError::Store(format!(
            "BlueGreenDeployment '{}' is missing metadata.namespace",
            name
        ))
    })?;
    info!("cleaning deleted BlueGreenDeployment '{}'", name);
    cleanup_inception_resources(&bgd, &ctx.client, &namespace).await?;
    cleanup_test_resources(&bgd, &ctx.client, &namespace).await?;
    delete_rollout_spec_snapshot(&bgd, &ctx.client, &namespace).await?;
    let removed = ctx
        .store
        .cleanup_blue_green(&blue_green_state_key(&namespace, &name))
        .await
        .map_err(|e| ReconcileError::Store(e.to_string()))?;
    if removed > 0 {
        info!(
            "cleaned {} store records for deleted BGD '{}'",
            removed, name
        );
    }
    remove_finalizer(&bgd, &ctx.client, &namespace).await?;
    Ok(())
}

async fn cleanup_orphaned_blue_green_refs(
    client: &kube::Client,
    store: &Arc<dyn StateStore>,
    lease: &LeaseConfig,
) -> std::result::Result<usize, ReconcileError> {
    let existing = existing_blue_green_keys(client).await?;
    let mut refs = store
        .list_blue_green_refs()
        .await
        .map_err(|e| ReconcileError::Store(e.to_string()))?;
    refs.extend(resources::blue_green_refs_from_owned_resources_all(client).await?);

    let mut cleaned = 0;
    for blue_green_ref in refs {
        if existing.contains(&blue_green_ref) {
            continue;
        }
        info!(
            "cleaning orphaned resources and store records for missing BGD '{}'",
            blue_green_ref
        );
        let cleanup_result = run_with_orphan_lease(
            &blue_green_ref,
            client.clone(),
            orphan_namespace(&blue_green_ref).unwrap_or("default"),
            lease,
            cleanup_single_orphaned_blue_green_ref(client, store, blue_green_ref.clone()),
        )
        .await?;
        if cleanup_result.is_some() {
            cleaned += 1;
        }
    }
    Ok(cleaned)
}

async fn cleanup_single_orphaned_blue_green_ref(
    client: &kube::Client,
    store: &Arc<dyn StateStore>,
    blue_green_key: String,
) -> std::result::Result<(), ReconcileError> {
    let (namespace, blue_green_ref) = split_blue_green_state_key(&blue_green_key)?;
    cleanup_orphaned_blue_green_resources(client, &namespace, &blue_green_ref).await?;
    let removed = store
        .cleanup_blue_green(&blue_green_key)
        .await
        .map_err(|e| ReconcileError::Store(e.to_string()))?;
    if removed > 0 {
        info!(
            "cleaned {} store records for orphaned BGD '{}'",
            removed, blue_green_key
        );
    }
    Ok(())
}

async fn sync_plugin_managers(
    client: &kube::Client,
    auth: &AuthConfig,
) -> std::result::Result<(), ReconcileError> {
    let plugins: Api<InceptionPlugin> = Api::all(client.clone());
    for plugin in plugins.list(&Default::default()).await?.items {
        if plugin.spec.manager.is_none() {
            continue;
        }
        plugin_lifecycle::invoke_plugin_manager_sync(client, &plugin, auth).await?;
    }
    Ok(())
}

async fn existing_blue_green_keys(
    client: &kube::Client,
) -> std::result::Result<BTreeSet<String>, ReconcileError> {
    let api: Api<BlueGreenDeployment> = Api::all(client.clone());
    Ok(api
        .list(&Default::default())
        .await?
        .items
        .into_iter()
        .filter_map(|bgd| {
            let namespace = bgd.namespace()?;
            let name = bgd.metadata.name?;
            Some(blue_green_state_key(&namespace, &name))
        })
        .collect())
}

pub(crate) fn blue_green_state_key(namespace: &str, name: &str) -> String {
    format!("{namespace}/{name}")
}

fn split_blue_green_state_key(key: &str) -> std::result::Result<(String, String), ReconcileError> {
    let Some((namespace, name)) = key.split_once('/') else {
        return Err(ReconcileError::Store(format!(
            "state key '{key}' is not namespace-qualified"
        )));
    };
    Ok((namespace.to_string(), name.to_string()))
}

fn orphan_namespace(key: &str) -> Option<&str> {
    key.split_once('/').map(|(namespace, _)| namespace)
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

fn active_rollout_has_new_spec_generation(bgd: &BlueGreenDeployment, phase: &BGDPhase) -> bool {
    if matches!(*phase, BGDPhase::Completed | BGDPhase::RolledBack) {
        return false;
    }
    let generation = bgd.metadata.generation.unwrap_or_default();
    let rollout_generation = bgd
        .status
        .as_ref()
        .and_then(|status| status.rollout_generation)
        .unwrap_or_default();
    rollout_generation > 0 && generation > rollout_generation
}

fn force_replace_active_rollout(bgd: &BlueGreenDeployment) -> bool {
    matches!(
        bgd.spec
            .update_policy
            .as_ref()
            .and_then(|policy| policy.active_rollout.as_ref()),
        Some(ActiveRolloutUpdatePolicy::ForceReplace)
    )
}

fn should_start_force_replace_rollout(bgd: &BlueGreenDeployment, phase: &BGDPhase) -> bool {
    force_replace_active_rollout(bgd)
        && active_rollout_has_new_spec_generation(bgd, phase)
        && !matches!(*phase, BGDPhase::Draining)
}

async fn start_force_replace_rollout(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
    auth: &AuthConfig,
) -> std::result::Result<(), ReconcileError> {
    let rollout_bgd = load_rollout_spec_snapshot(bgd, client, namespace)
        .await
        .unwrap_or_else(|err| {
            warn!(
                "falling back to latest BGD spec during force replace because rollout snapshot could not be loaded: {}",
                err
            );
            bgd.clone()
        });
    begin_draining_after_rollback(&rollout_bgd, client, namespace, auth).await?;
    update_status_phase(&rollout_bgd, client, namespace, BGDPhase::Draining).await;
    update_drain_started_at(&rollout_bgd, client, namespace).await;
    Ok(())
}

async fn ensure_rollout_spec_snapshot(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
) -> std::result::Result<(), ReconcileError> {
    let Some(name) = bgd.metadata.name.as_deref() else {
        return Ok(());
    };
    let snapshot_name = rollout_spec_snapshot_name(bgd, namespace);
    let api: Api<ConfigMap> = Api::namespaced(client.clone(), namespace);
    let spec = serde_yaml::to_string(&bgd.spec)
        .map_err(|err| ReconcileError::Store(format!("failed to snapshot BGD spec: {err}")))?;
    let rollout_generation = current_rollout_generation_from_bgd(bgd);
    let snapshot = ConfigMap {
        metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
            name: Some(snapshot_name.clone()),
            namespace: Some(namespace.to_string()),
            labels: Some(std::collections::BTreeMap::from([
                ("fluidbg.io/blue-green-ref".to_string(), name.to_string()),
                (
                    "fluidbg.io/rollout-snapshot".to_string(),
                    "true".to_string(),
                ),
            ])),
            ..Default::default()
        },
        data: Some(std::collections::BTreeMap::from([
            ("generation".to_string(), rollout_generation.to_string()),
            ("spec.yaml".to_string(), spec),
        ])),
        ..Default::default()
    };
    resources::apply_resource(api, &snapshot_name, &snapshot).await
}

async fn load_rollout_spec_snapshot(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
) -> std::result::Result<BlueGreenDeployment, ReconcileError> {
    let name = bgd.metadata.name.as_deref().unwrap_or("unknown");
    let snapshot_name = rollout_spec_snapshot_name(bgd, namespace);
    let api: Api<ConfigMap> = Api::namespaced(client.clone(), namespace);
    let snapshot = api.get(&snapshot_name).await.map_err(|err| {
        ReconcileError::Store(format!(
            "BGD '{name}' was updated during an active rollout, but rollout spec snapshot '{snapshot_name}' could not be loaded: {err}"
        ))
    })?;
    let spec_yaml = snapshot
        .data
        .as_ref()
        .and_then(|data| data.get("spec.yaml"))
        .ok_or_else(|| {
            ReconcileError::Store(format!(
                "BGD '{name}' rollout spec snapshot '{snapshot_name}' is missing spec.yaml"
            ))
        })?;
    let mut spec: BlueGreenDeploymentSpec = serde_yaml::from_str(spec_yaml).map_err(|err| {
        ReconcileError::Store(format!(
            "BGD '{name}' rollout spec snapshot '{snapshot_name}' could not be parsed: {err}"
        ))
    })?;
    if force_replace_active_rollout(bgd) {
        spec.update_policy = bgd.spec.update_policy.clone();
    }
    warn!(
        "BGD '{}' spec generation {} is deferred while rollout generation {} finishes",
        name,
        bgd.metadata.generation.unwrap_or_default(),
        current_rollout_generation_from_bgd(bgd)
    );
    let mut frozen = bgd.clone();
    frozen.spec = spec;
    Ok(frozen)
}

async fn delete_rollout_spec_snapshot(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
) -> std::result::Result<(), ReconcileError> {
    let snapshot_name = rollout_spec_snapshot_name(bgd, namespace);
    let api: Api<ConfigMap> = Api::namespaced(client.clone(), namespace);
    match api.delete(&snapshot_name, &Default::default()).await {
        Ok(_) => Ok(()),
        Err(kube::Error::Api(error)) if error.code == 404 => Ok(()),
        Err(err) => Err(ReconcileError::K8s(err)),
    }
}

fn current_rollout_generation_from_bgd(bgd: &BlueGreenDeployment) -> i64 {
    bgd.status
        .as_ref()
        .and_then(|status| status.rollout_generation)
        .or(bgd.metadata.generation)
        .unwrap_or_default()
}

fn rollout_spec_snapshot_name(bgd: &BlueGreenDeployment, namespace: &str) -> String {
    let name = bgd.metadata.name.as_deref().unwrap_or("unknown");
    let uid = bgd.metadata.uid.as_deref().unwrap_or("unknown");
    let mut hasher = Sha256::new();
    hasher.update(namespace.as_bytes());
    hasher.update(b"/");
    hasher.update(name.as_bytes());
    hasher.update(b"/");
    hasher.update(uid.as_bytes());
    let digest = hasher.finalize();
    let suffix = digest[..6]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let prefix_limit = 253usize.saturating_sub(suffix.len() + 1);
    let safe_name = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    let truncated = safe_name
        .trim_matches('-')
        .chars()
        .take(prefix_limit)
        .collect::<String>();
    let base = if truncated.is_empty() {
        "fluidbg-rollout".to_string()
    } else {
        truncated
    };
    format!("{base}-{suffix}")
}
