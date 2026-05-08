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

#[derive(Clone, Debug, Default)]
pub struct ControlPlaneServerTls {
    pub enabled: bool,
    pub cert_path: Option<String>,
    pub key_path: Option<String>,
}

impl ControlPlaneServerTls {
    pub fn from_env() -> Self {
        Self {
            enabled: env_flag("FLUIDBG_CONTROL_PLANE_TLS_ENABLED"),
            cert_path: optional_env("FLUIDBG_CONTROL_PLANE_TLS_CERT_PATH"),
            key_path: optional_env("FLUIDBG_CONTROL_PLANE_TLS_KEY_PATH"),
        }
    }
}

pub fn env_port(name: &str, default: u16) -> u16 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(default)
}

pub fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}

pub fn optional_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}
