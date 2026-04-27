use kube::api::Api;

use super::super::plugin_lifecycle::invoke_plugin_traffic_shift;
use super::super::promotion::initial_splitter_traffic_percent;
use super::super::status::update_status_progress;
use super::super::{AuthConfig, ReconcileError};
use crate::crd::blue_green::{BlueGreenDeployment, StrategyType};
use crate::crd::inception_plugin::{InceptionPlugin, PluginRole, Topology};

pub(in crate::controller) async fn validate_progressive_shifting_support(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
) -> std::result::Result<(), ReconcileError> {
    let Some(promotion) = bgd.spec.promotion.as_ref() else {
        return Ok(());
    };
    if !matches!(promotion.strategy.strategy_type, StrategyType::Progressive) {
        return Ok(());
    }

    let plugins: Api<InceptionPlugin> = Api::namespaced(client.clone(), namespace);
    let mut saw_splitter = false;
    for ip in &bgd.spec.inception_points {
        if !ip
            .roles
            .iter()
            .any(|role| matches!(role, PluginRole::Splitter))
        {
            continue;
        }
        saw_splitter = true;
        let plugin = plugins.get(&ip.plugin_ref.name).await?;
        validate_progressive_splitter_plugin(&ip.name, &ip.plugin_ref.name, &plugin)?;
    }

    if !saw_splitter {
        return Err(ReconcileError::Store(
            "progressive strategy requires at least one splitter inception point".to_string(),
        ));
    }

    Ok(())
}

pub(in crate::controller) fn validate_progressive_splitter_plugin(
    inception_point_name: &str,
    plugin_name: &str,
    plugin: &InceptionPlugin,
) -> std::result::Result<(), ReconcileError> {
    let supports_progressive = plugin
        .spec
        .features
        .as_ref()
        .map(|features| features.supports_progressive_shifting)
        .unwrap_or(false);
    if !supports_progressive {
        return Err(ReconcileError::Store(format!(
            "progressive strategy requires inception point '{}' plugin '{}' to set features.supportsProgressiveShifting=true",
            inception_point_name, plugin_name
        )));
    }
    if !matches!(plugin.spec.topology, Topology::Standalone) {
        return Err(ReconcileError::Store(format!(
            "progressive strategy requires standalone splitter plugin '{}'",
            plugin_name
        )));
    }
    Ok(())
}

pub(in crate::controller) async fn initialize_splitter_traffic(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
    auth: &AuthConfig,
) -> std::result::Result<(), ReconcileError> {
    let Some(traffic_percent) = initial_splitter_traffic_percent(bgd) else {
        return Ok(());
    };
    apply_splitter_traffic_percent(bgd, client, namespace, auth, traffic_percent).await?;
    update_status_progress(bgd, client, namespace, 0, traffic_percent).await;
    Ok(())
}

pub(in crate::controller) async fn apply_splitter_traffic_percent(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
    auth: &AuthConfig,
    traffic_percent: i32,
) -> std::result::Result<(), ReconcileError> {
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
        invoke_plugin_traffic_shift(
            client,
            bgd.metadata.name.as_deref().unwrap_or(""),
            namespace,
            &ip.name,
            &plugin,
            auth,
            traffic_percent as u8,
        )
        .await?;
    }

    Ok(())
}
