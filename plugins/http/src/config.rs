use anyhow::Result;
use fluidbg_plugin_sdk::{FilterCondition, NotificationFilter, TestIdSelector, TrafficRoute};
use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Config {
    #[serde(default)]
    #[allow(dead_code)]
    pub(crate) proxy_port: Option<u16>,
    #[serde(default)]
    pub(crate) port: Option<u16>,
    #[serde(default)]
    pub(crate) real_endpoint: Option<String>,
    #[serde(default)]
    pub(crate) target_url: Option<String>,
    #[serde(default)]
    pub(crate) green_endpoint: Option<String>,
    #[serde(default)]
    pub(crate) blue_endpoint: Option<String>,
    #[serde(default)]
    pub(crate) env_var_name: Option<String>,
    #[serde(default)]
    pub(crate) write_env_var: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub(crate) ingress_port: Option<u16>,
    #[serde(default)]
    #[allow(dead_code)]
    pub(crate) egress_port: Option<u16>,
    #[serde(default)]
    pub(crate) test_id: Option<TestIdSelector>,
    #[serde(default)]
    pub(crate) r#match: Vec<FilterCondition>,
    #[serde(default)]
    pub(crate) filters: Vec<NotificationFilter>,
    #[serde(default)]
    pub(crate) ingress: Option<DirectionConfig>,
    #[serde(default)]
    pub(crate) egress: Option<DirectionConfig>,
}

impl Config {
    pub(crate) fn listen_port(&self) -> u16 {
        self.port.unwrap_or(9090)
    }

    pub(crate) fn routed_proxy_target(&self, route: TrafficRoute) -> Option<&str> {
        match route {
            TrafficRoute::Blue => self
                .blue_endpoint
                .as_deref()
                .or(self.real_endpoint.as_deref()),
            TrafficRoute::Green => self
                .green_endpoint
                .as_deref()
                .or(self.real_endpoint.as_deref()),
            _ => self.real_endpoint.as_deref(),
        }
    }

    pub(crate) fn write_target(&self) -> Option<&str> {
        self.target_url.as_deref().or(self.real_endpoint.as_deref())
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DirectionConfig {
    #[serde(default)]
    pub(crate) filters: Vec<NotificationFilter>,
}

pub(crate) fn load_config() -> Result<Config> {
    fluidbg_plugin_sdk::load_yaml_config()
}
