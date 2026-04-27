use chrono::{DateTime, Utc};
use kube::api::Api;

use super::super::plugin_lifecycle::{
    PluginLifecycleStage, invoke_plugin_drain_status, invoke_plugin_lifecycle,
};
use super::super::resources::{cleanup_inception_resources, cleanup_test_resources};
use super::super::status::{update_inception_point_drain_statuses, update_status_phase};
use super::super::{AuthConfig, ReconcileError};
use crate::crd::blue_green::{
    BGDPhase, BlueGreenDeployment, InceptionPointDrainPhase, InceptionPointDrainStatus,
};
use crate::crd::inception_plugin::InceptionPlugin;

pub(in crate::controller) async fn reconcile_draining(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
    auth: &AuthConfig,
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
    let previous_statuses = bgd
        .status
        .as_ref()
        .map(|status| status.inception_point_drains.as_slice())
        .unwrap_or_default();

    for ip in &bgd.spec.inception_points {
        if let Some(existing) = previous_statuses.iter().find(|status| {
            status.name == ip.name
                && matches!(
                    status.phase,
                    InceptionPointDrainPhase::Successful
                        | InceptionPointDrainPhase::TimedOutMaybeSuccessful
                )
        }) {
            statuses.push(existing.clone());
            continue;
        }

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
            client,
            bgd.metadata.name.as_deref().unwrap_or(""),
            namespace,
            ip.name.as_str(),
            &plugin,
            auth,
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

    finalize_draining(bgd, client, namespace, auth).await
}

async fn finalize_draining(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
    auth: &AuthConfig,
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
            auth,
            PluginLifecycleStage::Cleanup,
        )
        .await?;
    }

    cleanup_inception_resources(bgd, client, namespace).await?;
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
