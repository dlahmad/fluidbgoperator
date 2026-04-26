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
use rand::distr::{Alphanumeric, SampleString};
use serde_json::json;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{error, info, warn};

use crate::crd::blue_green::{
    BGDPhase, BlueGreenDeployment, DeploymentRef, DeploymentSelector, ManagedDeploymentSpec,
};
use crate::crd::inception_plugin::{InceptionPlugin, PluginRole, Topology};
use crate::plugins::reconciler::{reconcile_inception_point, render_container_env_injections};
use crate::state_store::StateStore;
use crate::strategy::hard_switch::HardSwitchStrategy;
use crate::strategy::progressive::ProgressiveStrategy;
use crate::strategy::{PromotionAction, PromotionStrategy};
use crate::state_store::VerificationMode;

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
                    info!("BGD '{}' promotion threshold met! rate={:.4}", name, success_rate);
                    update_status_phase(&bgd, &client, &namespace, BGDPhase::Promoting).await;
                }
                PromotionAction::Rollback => {
                    info!("BGD '{}' rolling back: rate={:.4}", name, success_rate);
                    restore_after_rollback(&bgd, &client, &namespace).await?;
                    update_status_phase(&bgd, &client, &namespace, BGDPhase::RolledBack).await;
                    cleanup_test_resources(&bgd, &client, &namespace).await?;
                }
                PromotionAction::AdvanceStep {
                    step,
                    traffic_percent,
                } => {
                    apply_splitter_traffic_percent(&bgd, &client, &namespace, traffic_percent)
                        .await?;
                    update_status_progress(&bgd, &client, &namespace, step, traffic_percent)
                        .await;
                    info!(
                        "BGD '{}' advanced progressive splitter traffic to {}% (step {})",
                        name, traffic_percent, step
                    );
                }
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

async fn decide_promotion_action(
    bgd: &BlueGreenDeployment,
    data_counts: &crate::state_store::Counts,
    custom_counts: &crate::state_store::Counts,
    current_step: Option<i32>,
) -> std::result::Result<PromotionAction, ReconcileError> {
    let promotion = bgd.spec.promotion.as_ref().ok_or_else(|| {
        ReconcileError::Store("blue green deployment is missing promotion spec".into())
    })?;

    let data_action = if let Some(data) = promotion.data.as_ref() {
        Some(
            promotion_strategy_for(&bgd, data)?
                .decide(data_counts, current_step)
                .await,
        )
    } else {
        None
    };

    let custom_action = if let Some(custom) = promotion.custom.as_ref() {
        Some(decide_custom_promotion(custom_counts, custom))
    } else {
        None
    };

    match (data_action, custom_action) {
        (Some(PromotionAction::Rollback), _) | (_, Some(PromotionAction::Rollback)) => {
            Ok(PromotionAction::Rollback)
        }
        (Some(PromotionAction::ContinueObserving), _)
        | (_, Some(PromotionAction::ContinueObserving)) => Ok(PromotionAction::ContinueObserving),
        (Some(PromotionAction::AdvanceStep { step, traffic_percent }), None) => {
            Ok(PromotionAction::AdvanceStep {
                step,
                traffic_percent,
            })
        }
        (Some(PromotionAction::Promote), Some(PromotionAction::Promote))
        | (Some(PromotionAction::Promote), None)
        | (None, Some(PromotionAction::Promote)) => Ok(PromotionAction::Promote),
        (None, None) => Err(ReconcileError::Store(
            "promotion must define at least one of data or custom".to_string(),
        )),
        _ => Ok(PromotionAction::ContinueObserving),
    }
}

fn decide_custom_promotion(
    counts: &crate::state_store::Counts,
    _custom: &crate::crd::blue_green::CustomPromotionSpec,
) -> PromotionAction {
    if counts.pending > 0 {
        return PromotionAction::ContinueObserving;
    }
    if counts.failed > 0 || counts.timed_out > 0 {
        return PromotionAction::Rollback;
    }
    if counts.passed == 0 {
        return PromotionAction::ContinueObserving;
    }
    PromotionAction::Promote
}

fn promotion_strategy_for(
    bgd: &BlueGreenDeployment,
    data_promotion: &crate::crd::blue_green::DataPromotionSpec,
) -> std::result::Result<Box<dyn PromotionStrategy>, ReconcileError> {
    let promotion = bgd.spec.promotion.as_ref().ok_or_else(|| {
        ReconcileError::Store("blue green deployment is missing promotion spec".into())
    })?;

    match promotion.strategy.strategy_type {
        crate::crd::blue_green::StrategyType::HardSwitch => Ok(Box::new(
            HardSwitchStrategy::from_criteria(data_promotion),
        )),
        crate::crd::blue_green::StrategyType::Progressive => {
            let progressive = promotion.strategy.progressive.as_ref().ok_or_else(|| {
                ReconcileError::Store("progressive strategy selected without steps".into())
            })?;
            Ok(Box::new(ProgressiveStrategy::from_steps(
                progressive.steps.clone(),
                progressive.rollback_on_step_failure,
            )))
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

fn validate_test_configuration(bgd: &BlueGreenDeployment) -> std::result::Result<(), ReconcileError> {
    let Some(promotion) = bgd.spec.promotion.as_ref() else {
        if bgd.spec.tests.is_empty() {
            return Ok(());
        }
        return Err(ReconcileError::Store(
            "tests require a promotion spec".to_string(),
        ));
    };

    if promotion.data.is_none() && promotion.custom.is_none() {
        return Err(ReconcileError::Store(
            "promotion must define at least one of data or custom".to_string(),
        ));
    }

    for test in &bgd.spec.tests {
        if test.data_verification.is_none() && test.custom_verification.is_none() {
            return Err(ReconcileError::Store(format!(
                "test '{}' must define at least one of dataVerification or customVerification",
                test.name
            )));
        }
        if test.data_verification.is_some() && promotion.data.is_none() {
            return Err(ReconcileError::Store(format!(
                "test '{}' uses dataVerification but promotion.data is missing",
                test.name
            )));
        }
        if test.custom_verification.is_some() && promotion.custom.is_none() {
            return Err(ReconcileError::Store(format!(
                "test '{}' uses customVerification but promotion.custom is missing",
                test.name
            )));
        }
    }

    Ok(())
}

fn has_splitter(bgd: &BlueGreenDeployment) -> bool {
    bgd.spec
        .inception_points
        .iter()
        .any(|ip| ip.roles.iter().any(|role| matches!(role, PluginRole::Splitter)))
}

fn initial_splitter_traffic_percent(bgd: &BlueGreenDeployment) -> Option<i32> {
    if !has_splitter(bgd) {
        return None;
    }
    match bgd.spec.promotion.as_ref()?.strategy.strategy_type {
        crate::crd::blue_green::StrategyType::HardSwitch => Some(100),
        crate::crd::blue_green::StrategyType::Progressive => bgd
            .spec
            .promotion
            .as_ref()?
            .strategy
            .progressive
            .as_ref()?
            .steps
            .first()
            .map(|step| step.traffic_percent),
    }
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
        if !ip.roles.iter().any(|role| matches!(role, PluginRole::Splitter)) {
            continue;
        }
        let plugin = plugins.get(&ip.plugin_ref.name).await?;
        if !matches!(plugin.spec.topology, Topology::Standalone) {
            continue;
        }
        let deployment_name = format!("fluidbg-{}", ip.name);
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
) -> std::result::Result<(), ReconcileError> {
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

    delete_current_green(client, namespace, &candidate, &current_green).await?;
    Ok(())
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
        &deployment_namespace(&candidate_ref(bgd), namespace),
        &candidate_ref(bgd).name,
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
    bgd: &BlueGreenDeployment,
    default_namespace: &str,
    deployment_spec: &ManagedDeploymentSpec,
    family_labels: &BTreeMap<String, String>,
    is_green: bool,
) -> std::result::Result<(), ReconcileError> {
    let candidate = candidate_ref(bgd);
    let mut deployment = Deployment {
        metadata: ObjectMeta {
            name: Some(candidate.name.clone()),
            namespace: Some(deployment_namespace_spec(deployment_spec, default_namespace)),
            ..Default::default()
        },
        spec: Some(deployment_spec.spec.clone()),
        ..Default::default()
    };
    apply_family_labels_to_deployment(&mut deployment, family_labels)?;
    set_green_label(&mut deployment, is_green)?;

    let name = deployment
        .metadata
        .name
        .clone()
        .unwrap_or_else(|| candidate.name.clone());
    let deploy_namespace = deployment
        .metadata
        .namespace
        .clone()
        .unwrap_or_else(|| deployment_namespace_spec(deployment_spec, default_namespace));

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
        Some(resolve_current_green(client, namespace, &bgd.spec.selector).await?)
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
                &deployment_namespace(&candidate_ref(bgd), namespace),
                &candidate_ref(bgd).name,
                &injections.blue,
            )
            .await?;
            wait_for_deployment_ready(
                client,
                &DeploymentIdentity {
                    namespace: deployment_namespace(&candidate_ref(bgd), namespace),
                    name: candidate_ref(bgd).name.clone(),
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
                    deploy_api.replace(&name, &Default::default(), &adopted).await?;
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

    for _ in 0..32 {
        let suffix = {
            let mut rng = rand::rng();
            Alphanumeric.sample_string(&mut rng, 6).to_ascii_lowercase()
        };
        let max_base_len = 253usize.saturating_sub(suffix.len() + 1);
        let base = if bgd_name.len() > max_base_len {
            &bgd_name[..max_base_len]
        } else {
            bgd_name
        };
        let candidate_name = format!("{base}-{suffix}");

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

fn deployment_namespace_spec(deployment_spec: &ManagedDeploymentSpec, default_namespace: &str) -> String {
    deployment_spec
        .namespace
        .clone()
        .unwrap_or_else(|| default_namespace.to_string())
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
    let patch = json!({
        "status": {
            "phase": phase,
            "observedGeneration": bgd.metadata.generation
        }
    });
    let pp = PatchParams::default();
    if let Err(e) = api.patch_status(&name, &pp, &Patch::Merge(&patch)).await {
        warn!("failed to update status for '{}': {}", name, e);
    }
}

async fn reset_status_for_new_rollout(
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
            "currentTrafficPercent": null
        }
    });
    let pp = PatchParams::default();
    if let Err(e) = api.patch_status(&name, &pp, &Patch::Merge(&patch)).await {
        warn!("failed to reset rollout status for '{}': {}", name, e);
    }
}

async fn update_status_generated_deployment_name(
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
    let pp = PatchParams::default();
    if let Err(e) = api.patch_status(&name, &pp, &Patch::Merge(&patch)).await {
        warn!(
            "failed to update generated deployment name for '{}': {}",
            name, e
        );
    }
}

async fn update_status_counts(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
    counts: &crate::state_store::Counts,
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
    let pp = PatchParams::default();
    if let Err(e) = api.patch_status(&name, &pp, &Patch::Merge(&patch)).await {
        warn!("failed to update status counts for '{}': {}", name, e);
    }
}

async fn update_status_progress(
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
    let pp = PatchParams::default();
    if let Err(e) = api.patch_status(&name, &pp, &Patch::Merge(&patch)).await {
        warn!("failed to update status progress for '{}': {}", name, e);
    }
}
