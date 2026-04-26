use kube::api::Api;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::crd::blue_green::BlueGreenDeployment;
use crate::crd::inception_plugin::InceptionPlugin;
use crate::plugins::reconciler::inception_service_name;

use super::{ReconcileError, apply_assignments, wait_for_deployments_ready};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum AssignmentTarget {
    Green,
    Blue,
    Test,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum AssignmentKind {
    Env,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PropertyAssignment {
    pub(super) target: AssignmentTarget,
    pub(super) kind: AssignmentKind,
    pub(super) name: String,
    pub(super) value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) container_name: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PluginLifecycleResponse {
    #[serde(default)]
    pub(super) assignments: Vec<PropertyAssignment>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PluginDrainStatusResponse {
    pub(super) drained: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) message: Option<String>,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum PluginLifecycleStage {
    Prepare,
    Drain,
    Cleanup,
}

pub(super) async fn invoke_plugin_lifecycle(
    _client: &kube::Client,
    blue_green_ref: &str,
    namespace: &str,
    inception_point: &str,
    plugin: &InceptionPlugin,
    stage: PluginLifecycleStage,
) -> std::result::Result<Option<PluginLifecycleResponse>, ReconcileError> {
    let Some(lifecycle) = &plugin.spec.lifecycle else {
        return Ok(None);
    };
    let path = match stage {
        PluginLifecycleStage::Prepare => lifecycle.prepare_path.as_deref(),
        PluginLifecycleStage::Drain => lifecycle.drain_path.as_deref(),
        PluginLifecycleStage::Cleanup => lifecycle.cleanup_path.as_deref(),
    };
    let Some(path) = path else {
        return Ok(None);
    };
    let service_name = inception_service_name(blue_green_ref, inception_point);
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
                let payload = response
                    .json::<PluginLifecycleResponse>()
                    .await
                    .map_err(|e| {
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

pub(super) async fn invoke_plugin_drain_status(
    blue_green_ref: &str,
    namespace: &str,
    inception_point: &str,
    plugin: &InceptionPlugin,
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
        .container
        .ports
        .first()
        .map(|port| port.container_port)
        .unwrap_or(9090);
    let url = format!("http://{}.{}:{port}{path}", service_name, namespace);
    let response = reqwest::Client::new()
        .get(&url)
        .send()
        .await
        .map_err(|err| {
            ReconcileError::Store(format!("plugin drain status call failed for {url}: {err}"))
        })?
        .error_for_status()
        .map_err(|err| {
            ReconcileError::Store(format!("plugin drain status call failed for {url}: {err}"))
        })?;
    let payload = response
        .json::<PluginDrainStatusResponse>()
        .await
        .map_err(|err| {
            ReconcileError::Store(format!(
                "plugin drain status response deserialization failed for {url}: {err}"
            ))
        })?;
    Ok(Some(payload))
}

pub(super) async fn start_plugin_draining(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
    restore_targets: &[AssignmentTarget],
) -> std::result::Result<(), ReconcileError> {
    let plugins: Api<InceptionPlugin> = Api::namespaced(client.clone(), namespace);

    for ip in &bgd.spec.inception_points {
        let plugin = plugins.get(&ip.plugin_ref.name).await?;
        if let Some(assignments) = invoke_plugin_lifecycle(
            client,
            bgd.metadata.name.as_deref().unwrap_or(""),
            namespace,
            ip.name.as_str(),
            &plugin,
            PluginLifecycleStage::Drain,
        )
        .await?
        {
            let filtered = filter_assignments(&assignments.assignments, restore_targets);
            if !filtered.is_empty() {
                let touched = apply_assignments(bgd, client, namespace, &filtered, false).await?;
                wait_for_deployments_ready(client, &touched).await?;
            }
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
