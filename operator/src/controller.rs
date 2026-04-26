use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use futures::StreamExt;
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::EnvVar;
use kube::api::{Api, DeleteParams, ListParams};
use kube::runtime::controller::Action;
use kube::runtime::{Controller, watcher};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracing::{error, info, warn};

use crate::crd::blue_green::{
    BGDPhase, BlueGreenDeployment, DeploymentRef, DeploymentSelector, InceptionPointDrainPhase,
    InceptionPointDrainStatus, ManagedDeploymentSpec,
};
use crate::crd::inception_plugin::{InceptionPlugin, PluginRole, Topology};
use crate::plugins::reconciler::{inception_instance_base_name, reconcile_inception_point};
use crate::state_store::StateStore;
use crate::state_store::VerificationMode;
use crate::strategy::PromotionAction;

mod plugin_lifecycle;
mod promotion;
mod resources;
mod status;

#[cfg(test)]
mod tests;

use plugin_lifecycle::{
    AssignmentKind, AssignmentTarget, PluginLifecycleStage, PropertyAssignment,
    invoke_plugin_drain_status, invoke_plugin_lifecycle, start_plugin_draining,
};
use promotion::{
    decide_promotion_action, initial_splitter_traffic_percent, validate_test_configuration,
};
use resources::{
    apply_deployment_manifest, apply_resource, cleanup_test_resources, delete_deployment,
    ensure_inception_point_owned_resources, ensure_test_resources,
};
use status::{
    current_rollout_generation, ensure_rollout_generation, reset_status_for_new_rollout,
    update_drain_started_at, update_inception_point_drain_statuses, update_status_counts,
    update_status_generated_deployment_name, update_status_phase, update_status_progress,
};

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct DeploymentPatchKey {
    namespace: String,
    name: String,
    container_name: Option<String>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct DeploymentIdentity {
    namespace: String,
    name: String,
}

struct Ctx {
    client: kube::Client,
    namespace: String,
    store: Arc<dyn StateStore>,
}

#[derive(Debug, Error)]
pub(super) enum ReconcileError {
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

    if rollout_needs_restart(&bgd, &phase) {
        info!(
            "detected new spec generation for terminal BGD '{}'; restarting rollout",
            name
        );
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
            ensure_declared_deployments(&bgd, &client, &namespace).await?;
            ensure_inception_resources(&bgd, &client, &namespace).await?;
            ensure_test_resources(&bgd, &client, &namespace).await?;
            initialize_splitter_traffic(&bgd, &client, &namespace).await?;
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
                    begin_draining_after_rollback(&bgd, &client, &namespace).await?;
                    update_status_phase(&bgd, &client, &namespace, BGDPhase::Draining).await;
                    update_drain_started_at(&bgd, &client, &namespace).await;
                }
                PromotionAction::AdvanceStep {
                    step,
                    traffic_percent,
                } => {
                    apply_splitter_traffic_percent(&bgd, &client, &namespace, traffic_percent)
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
            begin_draining_after_promotion(&bgd, &client, &namespace, &previous_green).await?;
            update_status_phase(&bgd, &client, &namespace, BGDPhase::Draining).await;
            update_drain_started_at(&bgd, &client, &namespace).await;
            Ok(Action::requeue(std::time::Duration::from_secs(5)))
        }
        BGDPhase::Draining => {
            reconcile_draining(&bgd, &client, &namespace).await?;
            Ok(Action::requeue(std::time::Duration::from_secs(5)))
        }
        BGDPhase::Completed | BGDPhase::RolledBack => {
            info!("BGD '{}' in terminal state {:?}", name, phase);
            cleanup_test_resources(&bgd, &client, &namespace).await?;
            Ok(Action::requeue(std::time::Duration::from_secs(300)))
        }
    }
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

async fn initialize_splitter_traffic(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
) -> std::result::Result<(), ReconcileError> {
    let Some(traffic_percent) = initial_splitter_traffic_percent(bgd) else {
        return Ok(());
    };
    apply_splitter_traffic_percent(bgd, client, namespace, traffic_percent).await?;
    update_status_progress(bgd, client, namespace, 0, traffic_percent).await;
    Ok(())
}

async fn apply_splitter_traffic_percent(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
    traffic_percent: i32,
) -> std::result::Result<(), ReconcileError> {
    let mut touched = Vec::new();
    let plugins: Api<InceptionPlugin> = Api::namespaced(client.clone(), namespace);

    for ip in &bgd.spec.inception_points {
        if !ip
            .roles
            .iter()
            .any(|role| matches!(role, PluginRole::Splitter))
        {
            continue;
        }
        let plugin = plugins.get(&ip.plugin_ref.name).await?;
        if !matches!(plugin.spec.topology, Topology::Standalone) {
            continue;
        }
        let deployment_name =
            inception_instance_base_name(bgd.metadata.name.as_deref().unwrap_or(""), &ip.name);
        patch_targeted_deployment_env(
            client,
            namespace,
            &deployment_name,
            Some(&deployment_name),
            &[("FLUIDBG_TRAFFIC_PERCENT", &traffic_percent.to_string())],
            false,
        )
        .await?;
        touched.push(DeploymentIdentity {
            namespace: namespace.to_string(),
            name: deployment_name,
        });
    }

    wait_for_deployments_ready(client, &touched).await?;
    Ok(())
}

async fn promote(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
) -> std::result::Result<DeploymentRef, ReconcileError> {
    let current_green = resolve_current_green(client, namespace, &bgd.spec.selector).await?;
    let candidate = candidate_ref(bgd);
    let candidate_namespace = deployment_namespace(&candidate, namespace);

    let candidate_api: Api<Deployment> = Api::namespaced(client.clone(), &candidate_namespace);
    let mut candidate_deploy = candidate_api.get(&candidate.name).await?;
    apply_family_labels_to_deployment(&mut candidate_deploy, &bgd.spec.selector.match_labels)?;
    set_green_label(&mut candidate_deploy, true)?;

    candidate_api
        .replace(&candidate.name, &Default::default(), &candidate_deploy)
        .await?;

    info!(
        "promoted: deployment '{}/{}' now marked fluidbg.io/green=true",
        candidate_namespace, candidate.name
    );

    Ok(current_green)
}

async fn ensure_declared_deployments(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
) -> std::result::Result<(), ReconcileError> {
    apply_deployment_manifest(
        client,
        bgd,
        namespace,
        &bgd.spec.deployment,
        &bgd.spec.selector.match_labels,
        false,
    )
    .await?;
    Ok(())
}

async fn bootstrap_initial_green_if_empty(
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
    )
    .await?;

    info!(
        "bootstrapped initial green deployment '{}/{}' from spec.deployment because selector matched no existing deployments",
        deployment_namespace_spec(&bgd.spec.deployment, namespace),
        candidate_ref(bgd).name
    );
    Ok(true)
}

async fn begin_draining_after_promotion(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
    previous_green: &DeploymentRef,
) -> std::result::Result<(), ReconcileError> {
    start_plugin_draining(bgd, client, namespace, &[AssignmentTarget::Blue]).await?;
    delete_current_green(client, namespace, &candidate_ref(bgd), previous_green).await?;
    Ok(())
}

async fn begin_draining_after_rollback(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
) -> std::result::Result<(), ReconcileError> {
    start_plugin_draining(bgd, client, namespace, &[AssignmentTarget::Green]).await?;
    delete_deployment(
        client,
        &deployment_namespace(&candidate_ref(bgd), namespace),
        &candidate_ref(bgd).name,
    )
    .await?;
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
        .map(|test| format!("http://{}.{}:{}", test.name, namespace, test.port))
        .unwrap_or_else(|| "http://localhost:8080".to_string());
    let test_data_verify_path = bgd
        .spec
        .tests
        .first()
        .and_then(|test| test.data_verification.as_ref())
        .map(|verification| verification.verify_path.as_str());

    for ip in &bgd.spec.inception_points {
        let plugin = plugins.get(&ip.plugin_ref.name).await?;
        ensure_inception_point_owned_resources(client, namespace, ip).await?;
        let resources = reconcile_inception_point(
            &plugin,
            ip,
            namespace,
            operator_url,
            &test_container_url,
            test_data_verify_path,
            &candidate_ref(bgd).name,
            bgd.metadata.name.as_deref().unwrap_or(""),
        )
        .map_err(ReconcileError::Store)?;
        let mut plugin_deployments = Vec::new();

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
            let deployment_namespace = deployment
                .metadata
                .namespace
                .clone()
                .unwrap_or_else(|| namespace.to_string());
            apply_resource(
                Api::namespaced(client.clone(), &deployment_namespace),
                &name,
                &deployment,
            )
            .await?;
            plugin_deployments.push(DeploymentIdentity {
                namespace: deployment_namespace,
                name,
            });
        }
        for service in resources.services {
            let name = service
                .metadata
                .name
                .clone()
                .ok_or_else(|| ReconcileError::Store("generated Service has no name".into()))?;
            apply_resource(Api::namespaced(client.clone(), namespace), &name, &service).await?;
        }

        if !plugin_deployments.is_empty() {
            wait_for_deployments_ready(client, &plugin_deployments).await?;
        }

        let mut assignments = Vec::new();
        assignments.extend(resources.green_env_injections.into_iter().map(|env| {
            PropertyAssignment {
                target: AssignmentTarget::Green,
                kind: AssignmentKind::Env,
                name: env.name,
                value: env.value.unwrap_or_default(),
                container_name: None,
            }
        }));
        assignments.extend(resources.blue_env_injections.into_iter().map(|env| {
            PropertyAssignment {
                target: AssignmentTarget::Blue,
                kind: AssignmentKind::Env,
                name: env.name,
                value: env.value.unwrap_or_default(),
                container_name: None,
            }
        }));
        assignments.extend(resources.test_env_injections.into_iter().map(|env| {
            PropertyAssignment {
                target: AssignmentTarget::Test,
                kind: AssignmentKind::Env,
                name: env.name,
                value: env.value.unwrap_or_default(),
                container_name: None,
            }
        }));

        if let Some(mut lifecycle_assignments) = invoke_plugin_lifecycle(
            client,
            bgd.metadata.name.as_deref().unwrap_or(""),
            namespace,
            ip.name.as_str(),
            &plugin,
            PluginLifecycleStage::Prepare,
        )
        .await?
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

async fn reconcile_draining(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
) -> std::result::Result<(), ReconcileError> {
    let drain_started_at = bgd
        .status
        .as_ref()
        .and_then(|status| status.drain_started_at.as_deref())
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc))
        .unwrap_or_else(Utc::now);
    let elapsed_seconds = (Utc::now() - drain_started_at).num_seconds().max(0);

    let plugins: Api<InceptionPlugin> = Api::namespaced(client.clone(), namespace);
    let mut statuses = Vec::new();
    let mut all_final = true;

    for ip in &bgd.spec.inception_points {
        let max_wait_seconds = ip
            .drain
            .as_ref()
            .and_then(|drain| drain.max_wait_seconds)
            .unwrap_or(60);

        if elapsed_seconds >= max_wait_seconds {
            statuses.push(InceptionPointDrainStatus {
                name: ip.name.clone(),
                phase: InceptionPointDrainPhase::TimedOutMaybeSuccessful,
                message: Some(format!(
                    "max drain wait of {}s elapsed before plugin confirmed completion",
                    max_wait_seconds
                )),
                completed_at: Some(Utc::now().to_rfc3339()),
            });
            continue;
        }

        let plugin = plugins.get(&ip.plugin_ref.name).await?;
        let status = match invoke_plugin_drain_status(
            bgd.metadata.name.as_deref().unwrap_or(""),
            namespace,
            ip.name.as_str(),
            &plugin,
        )
        .await?
        {
            Some(plugin_status) if plugin_status.drained => InceptionPointDrainStatus {
                name: ip.name.clone(),
                phase: InceptionPointDrainPhase::Successful,
                message: plugin_status.message,
                completed_at: Some(Utc::now().to_rfc3339()),
            },
            Some(plugin_status) => {
                all_final = false;
                InceptionPointDrainStatus {
                    name: ip.name.clone(),
                    phase: InceptionPointDrainPhase::Pending,
                    message: plugin_status.message,
                    completed_at: None,
                }
            }
            None => InceptionPointDrainStatus {
                name: ip.name.clone(),
                phase: InceptionPointDrainPhase::Successful,
                message: Some("plugin does not expose drainStatusPath".to_string()),
                completed_at: Some(Utc::now().to_rfc3339()),
            },
        };
        statuses.push(status);
    }

    update_inception_point_drain_statuses(bgd, client, namespace, &statuses).await;

    if !all_final
        && statuses
            .iter()
            .any(|status| matches!(status.phase, InceptionPointDrainPhase::Pending))
    {
        return Ok(());
    }

    finalize_draining(bgd, client, namespace).await
}

async fn finalize_draining(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
) -> std::result::Result<(), ReconcileError> {
    let plugins: Api<InceptionPlugin> = Api::namespaced(client.clone(), namespace);
    for ip in &bgd.spec.inception_points {
        let plugin = plugins.get(&ip.plugin_ref.name).await?;
        let _ = invoke_plugin_lifecycle(
            client,
            bgd.metadata.name.as_deref().unwrap_or(""),
            namespace,
            ip.name.as_str(),
            &plugin,
            PluginLifecycleStage::Cleanup,
        )
        .await?;
    }

    cleanup_test_resources(bgd, client, namespace).await?;

    let final_phase = if bgd
        .status
        .as_ref()
        .and_then(|status| status.test_cases_failed)
        .unwrap_or_default()
        > 0
        || bgd
            .status
            .as_ref()
            .and_then(|status| status.test_cases_timed_out)
            .unwrap_or_default()
            > 0
    {
        BGDPhase::RolledBack
    } else {
        BGDPhase::Completed
    };
    update_status_phase(bgd, client, namespace, final_phase).await;
    Ok(())
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

async fn resolve_current_green(
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

fn apply_family_labels_to_deployment(
    deployment: &mut Deployment,
    labels_to_apply: &std::collections::BTreeMap<String, String>,
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

fn set_green_label(
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

fn candidate_ref(bgd: &BlueGreenDeployment) -> DeploymentRef {
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

fn generated_candidate_name_seed(bgd: &BlueGreenDeployment) -> String {
    let bgd_name = bgd.metadata.name.as_deref().unwrap_or("bgd");
    let uid = bgd.metadata.uid.as_deref().unwrap_or("");
    let generation = current_rollout_generation(bgd);
    format!("{bgd_name}:{uid}:{generation}")
}

fn candidate_name_with_suffix(base: &str, suffix: &str) -> String {
    let max_base_len = 253usize.saturating_sub(suffix.len() + 1);
    let trimmed_base = if base.len() > max_base_len {
        &base[..max_base_len]
    } else {
        base
    };
    format!("{trimmed_base}-{suffix}")
}

fn deterministic_candidate_suffixes(seed: &str) -> Vec<String> {
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

async fn ensure_generated_candidate_name(
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

fn deployment_namespace_spec(
    deployment_spec: &ManagedDeploymentSpec,
    default_namespace: &str,
) -> String {
    deployment_spec
        .namespace
        .clone()
        .unwrap_or_else(|| default_namespace.to_string())
}
