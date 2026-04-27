use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};
use std::time::Instant;

use anyhow::{Context, Result};
use fluidbg_plugin_sdk::{ObserverConfig, PluginInceptorRuntime, PluginRole};
use serde::Deserialize;
use tokio::sync::Mutex;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Config {
    #[serde(default)]
    pub(crate) connection_string: Option<String>,
    #[serde(default)]
    pub(crate) fully_qualified_namespace: Option<String>,
    #[serde(default)]
    pub(crate) auth: Option<AuthConfig>,
    #[serde(default)]
    pub(crate) management: Option<ManagementConfig>,
    #[serde(default = "default_drain_stability_seconds")]
    pub(crate) drain_stability_seconds: u64,
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

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AuthConfig {
    #[serde(default)]
    pub(crate) mode: Option<AuthMode>,
    #[serde(default)]
    pub(crate) tenant_id: Option<String>,
    #[serde(default)]
    pub(crate) client_id: Option<String>,
    #[serde(default)]
    pub(crate) federated_token_file: Option<String>,
    #[serde(default)]
    pub(crate) authority_host: Option<String>,
    #[serde(default)]
    pub(crate) sas_token_ttl_seconds: Option<i64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum AuthMode {
    ConnectionString,
    WorkloadIdentity,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ManagementConfig {
    #[serde(default)]
    pub(crate) subscription_id: Option<String>,
    #[serde(default)]
    pub(crate) resource_group: Option<String>,
    #[serde(default)]
    pub(crate) namespace_name: Option<String>,
    #[serde(default = "default_create_queues")]
    pub(crate) create_queues: bool,
    #[serde(default = "default_delete_queues")]
    pub(crate) delete_queues: bool,
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

#[derive(Debug, Deserialize)]
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
    pub(crate) runtime: PluginInceptorRuntime,
    pub(crate) service_bus: crate::servicebus::ServiceBusClient,
    mode: Arc<AtomicU8>,
    traffic_percent: Arc<AtomicU8>,
    drain_empty_since: Arc<Mutex<Option<Instant>>>,
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
        runtime: PluginInceptorRuntime,
        service_bus: crate::servicebus::ServiceBusClient,
    ) -> Self {
        Self {
            config,
            roles,
            runtime,
            service_bus,
            mode: Arc::new(AtomicU8::new(RuntimeMode::Active as u8)),
            traffic_percent: Arc::new(AtomicU8::new(blue_traffic_percent())),
            drain_empty_since: Arc::new(Mutex::new(None)),
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

    pub(crate) async fn stable_drained(&self, empty: bool) -> bool {
        if !empty {
            *self.drain_empty_since.lock().await = None;
            return false;
        }

        let required = std::time::Duration::from_secs(self.config.drain_stability_seconds);
        if required.is_zero() {
            return true;
        }

        let mut since = self.drain_empty_since.lock().await;
        let started = since.get_or_insert_with(Instant::now);
        started.elapsed() >= required
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

fn default_create_queues() -> bool {
    true
}

fn default_delete_queues() -> bool {
    true
}

fn default_drain_stability_seconds() -> u64 {
    65
}
