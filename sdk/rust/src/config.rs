use anyhow::Result;
use serde::de::DeserializeOwned;

use crate::models::PluginRole;

pub fn load_yaml_config<T: DeserializeOwned>() -> Result<T> {
    let path = std::env::var("FLUIDBG_CONFIG_PATH")
        .unwrap_or_else(|_| "/etc/fluidbg/config.yaml".to_string());
    let data = std::fs::read_to_string(path)?;
    Ok(serde_yaml_ng::from_str(&data)?)
}

pub fn active_roles() -> Vec<PluginRole> {
    std::env::var("FLUIDBG_ACTIVE_ROLES")
        .unwrap_or_default()
        .split(',')
        .filter_map(PluginRole::parse)
        .collect()
}

pub fn has_role(roles: &[PluginRole], role: PluginRole) -> bool {
    roles.contains(&role)
}

pub fn traffic_percent_from_env() -> u8 {
    std::env::var("FLUIDBG_TRAFFIC_PERCENT")
        .ok()
        .and_then(|value| value.parse::<u8>().ok())
        .unwrap_or(100)
        .min(100)
}
