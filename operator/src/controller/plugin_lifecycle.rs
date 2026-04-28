use kube::api::Api;
use tracing::warn;

use crate::crd::blue_green::BlueGreenDeployment;
use crate::crd::inception_plugin::InceptionPlugin;
use crate::plugins::reconciler::{
    ReconcileInceptionContext, inception_service_name, plugin_template_context,
    render_container_env_injections, secured_inception_config,
};

use super::deployments::{apply_assignments, wait_for_deployments_ready};
use super::resources::{sign_inception_auth_token, sign_manager_sync_auth_token};
use super::{AuthConfig, ReconcileError, validate_plugin_auth};

pub(super) use fluidbg_plugin_sdk::{
    AUTHORIZATION_HEADER, ActiveInception, AssignmentKind, AssignmentTarget,
    PluginDrainStatusResponse, PluginLifecycleResponse, PluginManagerLifecycleRequest,
    PluginManagerSyncRequest, PropertyAssignment, bearer_value,
};

#[derive(Clone, Copy, Debug)]
pub(super) enum PluginLifecycleStage {
    Prepare,
    Activate,
    Drain,
    Cleanup,
}

pub(super) async fn invoke_inceptor_lifecycle(
    client: &kube::Client,
    blue_green_ref: &str,
    namespace: &str,
    inception_point: &str,
    plugin: &InceptionPlugin,
    auth: &AuthConfig,
    stage: PluginLifecycleStage,
) -> std::result::Result<Option<PluginLifecycleResponse>, ReconcileError> {
    let Some(lifecycle) = &plugin.spec.lifecycle else {
        return Ok(None);
    };
    let path = match stage {
        PluginLifecycleStage::Prepare => lifecycle.prepare_path.as_deref(),
        PluginLifecycleStage::Activate => lifecycle.activate_path.as_deref(),
        PluginLifecycleStage::Drain => lifecycle.drain_path.as_deref(),
        PluginLifecycleStage::Cleanup => lifecycle.cleanup_path.as_deref(),
    };
    let Some(path) = path else {
        return Ok(None);
    };
    let service_name = inception_service_name(blue_green_ref, inception_point);
    let port = plugin
        .spec
        .inceptor
        .ports
        .first()
        .map(|port| port.container_port)
        .unwrap_or(9090);
    let url = format!("http://{}.{}:{port}{path}", service_name, namespace);
    let http = reqwest::Client::new();
    let auth_token = sign_inception_auth_token(
        client,
        namespace,
        auth,
        blue_green_ref,
        inception_point,
        plugin,
    )
    .await?;

    for attempt in 1..=10 {
        match http
            .post(&url)
            .header(AUTHORIZATION_HEADER, bearer_value(&auth_token))
            .send()
            .await
        {
            Ok(response) => {
                let response = response.error_for_status().map_err(|e| {
                    ReconcileError::Store(format!(
                        "plugin inceptor lifecycle call failed for {url}: {e}"
                    ))
                })?;
                let payload = response
                    .json::<PluginLifecycleResponse>()
                    .await
                    .map_err(|e| {
                        ReconcileError::Store(format!(
                            "plugin inceptor lifecycle response deserialization failed for {url}: {e}"
                        ))
                    })?;
                return Ok(Some(payload));
            }
            Err(err) if attempt < 10 => {
                warn!(
                    "plugin inceptor lifecycle endpoint {} not ready yet (attempt {}/10): {}",
                    url, attempt, err
                );
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            Err(err) => {
                return Err(ReconcileError::Store(format!(
                    "plugin inceptor lifecycle call failed for {url}: {err}"
                )));
            }
        }
    }

    Ok(None)
}

pub(super) async fn invoke_plugin_manager_sync(
    client: &kube::Client,
    plugin: &InceptionPlugin,
    auth: &AuthConfig,
) -> std::result::Result<(), ReconcileError> {
    let Some(manager) = &plugin.spec.manager else {
        return Ok(());
    };
    let path = manager.sync_path.as_deref().unwrap_or("/manager/sync");
    let manager_namespace = manager.namespace.as_deref().unwrap_or("fluidbg-system");
    let port = manager.port.unwrap_or(9090);
    let url = format!(
        "http://{}.{}:{port}{path}",
        manager.service_name, manager_namespace
    );
    let plugin_name = plugin.metadata.name.clone().unwrap_or_default();
    let payload = PluginManagerSyncRequest {
        plugin: plugin_name.clone(),
        active_inceptions: active_inceptions_for_plugin(client, &plugin_name).await?,
    };
    let auth_token = sign_manager_sync_auth_token(client, auth, plugin).await?;
    let http = reqwest::Client::new();
    for attempt in 1..=3 {
        match http
            .post(&url)
            .header(AUTHORIZATION_HEADER, bearer_value(&auth_token))
            .json(&payload)
            .send()
            .await
        {
            Ok(response) => {
                response.error_for_status().map_err(|err| {
                    ReconcileError::Store(format!("plugin manager sync failed for {url}: {err}"))
                })?;
                return Ok(());
            }
            Err(err) if attempt < 3 => {
                warn!(
                    "plugin manager sync endpoint {} not ready yet (attempt {}/3): {}",
                    url, attempt, err
                );
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            Err(err) => {
                return Err(ReconcileError::Store(format!(
                    "plugin manager sync failed for {url}: {err}"
                )));
            }
        }
    }
    Ok(())
}

pub(super) async fn invoke_plugin_manager_lifecycle(
    client: &kube::Client,
    blue_green_ref: &str,
    namespace: &str,
    inception_point: &crate::crd::blue_green::InceptionPoint,
    plugin: &InceptionPlugin,
    auth: &AuthConfig,
    stage: PluginLifecycleStage,
) -> std::result::Result<Option<PluginLifecycleResponse>, ReconcileError> {
    let Some(manager) = &plugin.spec.manager else {
        return Ok(None);
    };
    let path = match stage {
        PluginLifecycleStage::Prepare => manager
            .prepare_path
            .as_deref()
            .unwrap_or("/manager/prepare"),
        PluginLifecycleStage::Activate => return Ok(None),
        PluginLifecycleStage::Cleanup => manager
            .cleanup_path
            .as_deref()
            .unwrap_or("/manager/cleanup"),
        PluginLifecycleStage::Drain => return Ok(None),
    };
    let manager_namespace = manager.namespace.as_deref().unwrap_or("fluidbg-system");
    let port = manager.port.unwrap_or(9090);
    let url = format!(
        "http://{}.{}:{port}{path}",
        manager.service_name, manager_namespace
    );
    let auth_token = sign_inception_auth_token(
        client,
        namespace,
        auth,
        blue_green_ref,
        &inception_point.name,
        plugin,
    )
    .await?;
    let bearer = bearer_value(&auth_token);
    let claims = validate_plugin_auth(client, auth, Some(&bearer))
        .await
        .map_err(ReconcileError::Store)?
        .ok_or_else(|| ReconcileError::Store("manager auth token did not produce claims".into()))?;
    let blue_green_uid = claims.blue_green_uid.clone().unwrap_or_default();
    let context = ReconcileInceptionContext {
        namespace,
        operator_url: "",
        test_container_url: "",
        test_data_verify_path: None,
        blue_deployment_name: "",
        blue_green_ref,
        blue_green_uid: &blue_green_uid,
        auth_token: &auth_token,
        manager_inceptor_env: &[],
    };
    let payload = PluginManagerLifecycleRequest {
        namespace: namespace.to_string(),
        blue_green_ref: blue_green_ref.to_string(),
        blue_green_uid: claims.blue_green_uid,
        inception_point: inception_point.name.clone(),
        plugin: plugin.metadata.name.clone().unwrap_or_default(),
        roles: inception_point
            .roles
            .iter()
            .map(crate::plugins::reconciler::plugin_role_name)
            .map(str::to_string)
            .collect(),
        config: secured_inception_config(plugin, inception_point, &context),
        active_inceptions: active_inceptions_for_plugin(
            client,
            plugin.metadata.name.as_deref().unwrap_or_default(),
        )
        .await?,
    };
    let http = reqwest::Client::new();
    for attempt in 1..=10 {
        match http
            .post(&url)
            .header(AUTHORIZATION_HEADER, bearer_value(&auth_token))
            .json(&payload)
            .send()
            .await
        {
            Ok(response) => {
                let response = response.error_for_status().map_err(|e| {
                    ReconcileError::Store(format!("plugin manager call failed for {url}: {e}"))
                })?;
                let value = response.json::<serde_json::Value>().await.map_err(|e| {
                    ReconcileError::Store(format!(
                        "plugin manager response deserialization failed for {url}: {e}"
                    ))
                })?;
                let payload =
                    serde_json::from_value::<PluginLifecycleResponse>(value).unwrap_or_default();
                return Ok(Some(payload));
            }
            Err(err) if attempt < 10 => {
                warn!(
                    "plugin manager endpoint {} not ready yet (attempt {}/10): {}",
                    url, attempt, err
                );
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            Err(err) => {
                return Err(ReconcileError::Store(format!(
                    "plugin manager call failed for {url}: {err}"
                )));
            }
        }
    }
    Ok(None)
}

pub(super) async fn active_inceptions_for_plugin(
    client: &kube::Client,
    plugin_name: &str,
) -> std::result::Result<Vec<ActiveInception>, ReconcileError> {
    let api: Api<BlueGreenDeployment> = Api::all(client.clone());
    let bgds = api.list(&Default::default()).await?;
    let mut active = Vec::new();
    for bgd in bgds {
        let phase = bgd.status.as_ref().and_then(|status| status.phase.as_ref());
        if matches!(
            phase,
            Some(
                crate::crd::blue_green::BGDPhase::Completed
                    | crate::crd::blue_green::BGDPhase::RolledBack
            )
        ) {
            continue;
        }
        let Some(namespace) = bgd.metadata.namespace.as_deref() else {
            continue;
        };
        let Some(blue_green_ref) = bgd.metadata.name.as_deref() else {
            continue;
        };
        for ip in &bgd.spec.inception_points {
            if ip.plugin_ref.name != plugin_name {
                continue;
            }
            active.push(ActiveInception {
                namespace: namespace.to_string(),
                blue_green_ref: blue_green_ref.to_string(),
                blue_green_uid: bgd.metadata.uid.clone(),
                inception_point: ip.name.clone(),
                plugin: plugin_name.to_string(),
                roles: ip
                    .roles
                    .iter()
                    .map(crate::plugins::reconciler::plugin_role_name)
                    .map(str::to_string)
                    .collect(),
                config: ip.config.clone(),
            });
        }
    }
    Ok(active)
}

pub(super) async fn invoke_inceptor_drain_status(
    client: &kube::Client,
    blue_green_ref: &str,
    namespace: &str,
    inception_point: &str,
    plugin: &InceptionPlugin,
    auth: &AuthConfig,
) -> std::result::Result<Option<PluginDrainStatusResponse>, ReconcileError> {
    let Some(lifecycle) = &plugin.spec.lifecycle else {
        return Ok(None);
    };
    let Some(path) = lifecycle.drain_status_path.as_deref() else {
        return Ok(None);
    };
    let service_name = inception_service_name(blue_green_ref, inception_point);
    let port = plugin
        .spec
        .inceptor
        .ports
        .first()
        .map(|port| port.container_port)
        .unwrap_or(9090);
    let url = format!("http://{}.{}:{port}{path}", service_name, namespace);
    let auth_token = sign_inception_auth_token(
        client,
        namespace,
        auth,
        blue_green_ref,
        inception_point,
        plugin,
    )
    .await?;
    let response = reqwest::Client::new()
        .get(&url)
        .header(AUTHORIZATION_HEADER, bearer_value(&auth_token))
        .send()
        .await
        .map_err(|err| {
            ReconcileError::Store(format!(
                "plugin inceptor drain status call failed for {url}: {err}"
            ))
        })?
        .error_for_status()
        .map_err(|err| {
            ReconcileError::Store(format!(
                "plugin inceptor drain status call failed for {url}: {err}"
            ))
        })?;
    let payload = response
        .json::<PluginDrainStatusResponse>()
        .await
        .map_err(|err| {
            ReconcileError::Store(format!(
                "plugin inceptor drain status response deserialization failed for {url}: {err}"
            ))
        })?;
    Ok(Some(payload))
}

pub(super) async fn invoke_inceptor_traffic_shift(
    client: &kube::Client,
    blue_green_ref: &str,
    namespace: &str,
    inception_point: &str,
    plugin: &InceptionPlugin,
    auth: &AuthConfig,
    traffic_percent: u8,
) -> std::result::Result<(), ReconcileError> {
    let path = plugin
        .spec
        .lifecycle
        .as_ref()
        .and_then(|lifecycle| lifecycle.traffic_shift_path.as_deref())
        .unwrap_or("/traffic");
    let service_name = inception_service_name(blue_green_ref, inception_point);
    let port = plugin
        .spec
        .inceptor
        .ports
        .first()
        .map(|port| port.container_port)
        .unwrap_or(9090);
    let url = format!("http://{}.{}:{port}{path}", service_name, namespace);
    let http = reqwest::Client::new();
    let auth_token = sign_inception_auth_token(
        client,
        namespace,
        auth,
        blue_green_ref,
        inception_point,
        plugin,
    )
    .await?;
    for attempt in 1..=10 {
        match http
            .post(&url)
            .header(AUTHORIZATION_HEADER, bearer_value(&auth_token))
            .json(&fluidbg_plugin_sdk::TrafficShiftRequest { traffic_percent })
            .send()
            .await
        {
            Ok(response) => {
                response.error_for_status().map_err(|err| {
                    ReconcileError::Store(format!(
                        "plugin inceptor traffic shift call failed for {url}: {err}"
                    ))
                })?;
                return Ok(());
            }
            Err(err) if attempt < 10 => {
                warn!(
                    "plugin inceptor traffic shift endpoint {} not ready yet (attempt {}/10): {}",
                    url, attempt, err
                );
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            Err(err) => {
                return Err(ReconcileError::Store(format!(
                    "plugin inceptor traffic shift call failed for {url}: {err}"
                )));
            }
        }
    }
    Ok(())
}

pub(super) async fn start_plugin_draining(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
    auth: &AuthConfig,
    restore_targets: &[AssignmentTarget],
) -> std::result::Result<(), ReconcileError> {
    let plugins: Api<InceptionPlugin> = Api::namespaced(client.clone(), namespace);

    for ip in &bgd.spec.inception_points {
        let plugin = plugins.get(&ip.plugin_ref.name).await?;
        let template_context =
            plugin_template_context(ip, namespace, bgd.metadata.name.as_deref().unwrap_or(""));
        let restore_injections = render_container_env_injections(&plugin, &template_context, true);
        let mut assignments = Vec::new();
        assignments.extend(
            restore_injections
                .green
                .into_iter()
                .map(|env| PropertyAssignment {
                    target: AssignmentTarget::Green,
                    kind: AssignmentKind::Env,
                    name: env.name,
                    value: env.value.unwrap_or_default(),
                    container_name: None,
                }),
        );
        assignments.extend(
            restore_injections
                .blue
                .into_iter()
                .map(|env| PropertyAssignment {
                    target: AssignmentTarget::Blue,
                    kind: AssignmentKind::Env,
                    name: env.name,
                    value: env.value.unwrap_or_default(),
                    container_name: None,
                }),
        );
        assignments.extend(
            restore_injections
                .test
                .into_iter()
                .map(|env| PropertyAssignment {
                    target: AssignmentTarget::Test,
                    kind: AssignmentKind::Env,
                    name: env.name,
                    value: env.value.unwrap_or_default(),
                    container_name: None,
                }),
        );
        if let Some(lifecycle_response) = invoke_inceptor_lifecycle(
            client,
            bgd.metadata.name.as_deref().unwrap_or(""),
            namespace,
            ip.name.as_str(),
            &plugin,
            auth,
            PluginLifecycleStage::Drain,
        )
        .await?
        {
            assignments.extend(lifecycle_response.assignments);
        }
        let filtered = filter_assignments(&assignments, restore_targets);
        if !filtered.is_empty() {
            let touched = apply_assignments(bgd, client, namespace, &filtered, false).await?;
            wait_for_deployments_ready(client, &touched).await?;
        }
    }

    Ok(())
}

fn filter_assignments(
    assignments: &[PropertyAssignment],
    targets: &[AssignmentTarget],
) -> Vec<PropertyAssignment> {
    assignments
        .iter()
        .filter(|assignment| targets.contains(&assignment.target))
        .cloned()
        .collect()
}
