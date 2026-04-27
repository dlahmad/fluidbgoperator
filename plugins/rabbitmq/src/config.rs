use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};

use anyhow::{Context, Result};
use fluidbg_plugin_sdk::{ObserverConfig, PluginInceptorRuntime, PluginRole};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Config {
    #[serde(default)]
    pub(crate) amqp_url: Option<String>,
    #[serde(default)]
    pub(crate) management: Option<ManagementConfig>,
    #[serde(default)]
    pub(crate) queue_declaration: QueueDeclarationConfig,
    #[serde(default)]
    pub(crate) shadow_queue: Option<ShadowQueueConfig>,
    #[serde(default)]
    pub(crate) duplicator: Option<DuplicatorConfig>,
    #[serde(default)]
    pub(crate) splitter: Option<SplitterConfig>,
    #[serde(default)]
    pub(crate) combiner: Option<CombinerConfig>,
    #[serde(default)]
    pub(crate) writer: Option<WriterConfig>,
    #[serde(default)]
    pub(crate) consumer: Option<ConsumerConfig>,
    #[serde(default)]
    pub(crate) observer: Option<ObserverConfig>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct QueueDeclarationConfig {
    #[serde(default)]
    pub(crate) durable: Option<bool>,
    #[serde(default)]
    pub(crate) exclusive: Option<bool>,
    #[serde(default)]
    pub(crate) auto_delete: Option<bool>,
    #[serde(default)]
    pub(crate) arguments: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ShadowQueueConfig {
    pub(crate) suffix: String,
    #[serde(default)]
    pub(crate) queue_declaration: QueueDeclarationConfig,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ManagementConfig {
    #[serde(default)]
    pub(crate) url: Option<String>,
    #[serde(default)]
    pub(crate) username: Option<String>,
    #[serde(default)]
    pub(crate) password: Option<String>,
    #[serde(default)]
    pub(crate) vhost: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SplitterConfig {
    pub(crate) input_queue: Option<String>,
    pub(crate) green_input_queue: Option<String>,
    pub(crate) blue_input_queue: Option<String>,
    pub(crate) green_input_queue_env_var: Option<String>,
    pub(crate) blue_input_queue_env_var: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DuplicatorConfig {
    pub(crate) input_queue: Option<String>,
    pub(crate) green_input_queue: Option<String>,
    pub(crate) blue_input_queue: Option<String>,
    pub(crate) green_input_queue_env_var: Option<String>,
    pub(crate) blue_input_queue_env_var: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CombinerConfig {
    pub(crate) output_queue: Option<String>,
    pub(crate) green_output_queue: Option<String>,
    pub(crate) blue_output_queue: Option<String>,
    pub(crate) green_output_queue_env_var: Option<String>,
    pub(crate) blue_output_queue_env_var: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WriterConfig {
    pub(crate) target_queue: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConsumerConfig {
    pub(crate) input_queue: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
#[allow(dead_code)]
pub(crate) struct WriteRequest {
    pub(crate) test_id: Option<String>,
    pub(crate) payload: serde_json::Value,
    pub(crate) properties: Option<serde_json::Value>,
}

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) config: Config,
    pub(crate) roles: Vec<PluginRole>,
    pub(crate) amqp_url: String,
    pub(crate) runtime: PluginInceptorRuntime,
    mode: Arc<AtomicU8>,
    traffic_percent: Arc<AtomicU8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RuntimeMode {
    Active = 0,
    Draining = 1,
    Idle = 2,
}

impl RuntimeMode {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Draining,
            2 => Self::Idle,
            _ => Self::Active,
        }
    }
}

impl AppState {
    pub(crate) fn new(
        config: Config,
        roles: Vec<PluginRole>,
        amqp_url: String,
        runtime: PluginInceptorRuntime,
    ) -> Self {
        Self {
            config,
            roles,
            amqp_url,
            runtime,
            mode: Arc::new(AtomicU8::new(RuntimeMode::Idle as u8)),
            traffic_percent: Arc::new(AtomicU8::new(blue_traffic_percent())),
        }
    }

    pub(crate) fn runtime_mode(&self) -> RuntimeMode {
        RuntimeMode::from_u8(self.mode.load(Ordering::Relaxed))
    }

    pub(crate) fn set_runtime_mode(&self, mode: RuntimeMode) {
        self.mode.store(mode as u8, Ordering::Relaxed);
    }

    pub(crate) fn traffic_percent(&self) -> u8 {
        self.traffic_percent.load(Ordering::Relaxed)
    }

    pub(crate) fn set_traffic_percent(&self, percent: u8) {
        self.traffic_percent
            .store(percent.min(100), Ordering::Relaxed);
    }
}

pub(crate) fn load_config() -> Result<Config> {
    fluidbg_plugin_sdk::load_yaml_config()
}

pub(crate) fn has_role(roles: &[PluginRole], role: PluginRole) -> bool {
    fluidbg_plugin_sdk::has_role(roles, role)
}

pub(crate) fn required<'a>(value: &'a Option<String>, name: &str) -> Result<&'a str> {
    value
        .as_deref()
        .with_context(|| format!("missing config field '{name}'"))
}

pub(crate) fn duplicator_config(config: &Config) -> Result<&DuplicatorConfig> {
    config
        .duplicator
        .as_ref()
        .context("missing config block 'duplicator'")
}

pub(crate) fn splitter_config(config: &Config) -> Result<&SplitterConfig> {
    config
        .splitter
        .as_ref()
        .context("missing config block 'splitter'")
}

pub(crate) fn combiner_config(config: &Config) -> Result<&CombinerConfig> {
    config
        .combiner
        .as_ref()
        .context("missing config block 'combiner'")
}

pub(crate) fn writer_config(config: &Config) -> Result<&WriterConfig> {
    config
        .writer
        .as_ref()
        .context("missing config block 'writer'")
}

pub(crate) fn consumer_config(config: &Config) -> Result<&ConsumerConfig> {
    config
        .consumer
        .as_ref()
        .context("missing config block 'consumer'")
}

pub(crate) fn observer_config(config: &Config) -> Option<&ObserverConfig> {
    config.observer.as_ref()
}

pub(crate) fn blue_traffic_percent() -> u8 {
    fluidbg_plugin_sdk::traffic_percent_from_env()
}

pub(crate) fn routes_to_blue(payload: &[u8], traffic_percent: u8) -> bool {
    fluidbg_plugin_sdk::routes_to_blue(payload, traffic_percent)
}

pub(crate) fn inceptor_infra_disabled() -> bool {
    std::env::var("FLUIDBG_INCEPTOR_INFRA_DISABLED").as_deref() == Ok("true")
}

pub(crate) fn shadow_queue_name(config: &Config, queue: &str) -> Option<String> {
    config
        .shadow_queue
        .as_ref()
        .map(|shadow| fluidbg_plugin_sdk::derived_shadow_queue_name(queue, &shadow.suffix))
}
