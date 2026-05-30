use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};

use anyhow::{Context, Result};
use fluidbg_plugin_sdk::{ObserverConfig, PluginInceptorRuntime, PluginRole};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use tokio::sync::Notify;

pub(crate) const CORE_READY_INPUT: u8 = 0b0000_0001;
pub(crate) const CORE_READY_GREEN_OUTPUT: u8 = 0b0000_0010;
pub(crate) const CORE_READY_BLUE_OUTPUT: u8 = 0b0000_0100;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    #[serde(default)]
    pub(crate) mode: NatsMode,
    #[serde(default)]
    pub(crate) stream: StreamConfig,
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

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum NatsMode {
    #[default]
    JetStream,
    Core,
}

impl NatsMode {
    pub(crate) fn is_core(self) -> bool {
        matches!(self, Self::Core)
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StreamConfig {
    #[serde(default)]
    pub(crate) subjects: Vec<String>,
    #[serde(default)]
    pub(crate) storage: Option<String>,
    #[serde(default)]
    pub(crate) retention: Option<String>,
    #[serde(default)]
    pub(crate) replicas: Option<usize>,
    #[serde(default)]
    pub(crate) max_messages: Option<i64>,
    #[serde(default)]
    pub(crate) max_bytes: Option<i64>,
    #[serde(default)]
    pub(crate) max_age_seconds: Option<i64>,
    #[serde(default)]
    pub(crate) discard: Option<String>,
    #[serde(default)]
    pub(crate) allow_rollup: Option<bool>,
    #[serde(default)]
    pub(crate) deny_delete: Option<bool>,
    #[serde(default)]
    pub(crate) deny_purge: Option<bool>,
    #[serde(default)]
    pub(crate) placement: Option<PlacementConfig>,
    #[serde(default)]
    pub(crate) persistence_mode: Option<String>,
    #[serde(default)]
    pub(crate) subject_transform: Option<SubjectTransformConfig>,
    #[serde(default)]
    pub(crate) metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PlacementConfig {
    #[serde(default)]
    pub(crate) cluster: Option<String>,
    #[serde(default)]
    pub(crate) tags: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SubjectTransformConfig {
    pub(crate) source: String,
    pub(crate) destination: String,
}

#[derive(Clone, Debug, Deserialize)]
#[allow(dead_code)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SplitterConfig {
    pub(crate) input_subject: Option<String>,
    pub(crate) queue_group: Option<String>,
    pub(crate) green_input_subject: Option<String>,
    pub(crate) blue_input_subject: Option<String>,
    pub(crate) green_input_subject_env_var: Option<String>,
    pub(crate) blue_input_subject_env_var: Option<String>,
    pub(crate) green_queue_group: Option<String>,
    pub(crate) blue_queue_group: Option<String>,
    pub(crate) green_queue_group_env_var: Option<String>,
    pub(crate) blue_queue_group_env_var: Option<String>,
    pub(crate) temporary_subject_identifier: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[allow(dead_code)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DuplicatorConfig {
    pub(crate) input_subject: Option<String>,
    pub(crate) queue_group: Option<String>,
    pub(crate) green_input_subject: Option<String>,
    pub(crate) blue_input_subject: Option<String>,
    pub(crate) green_input_subject_env_var: Option<String>,
    pub(crate) blue_input_subject_env_var: Option<String>,
    pub(crate) green_queue_group: Option<String>,
    pub(crate) blue_queue_group: Option<String>,
    pub(crate) green_queue_group_env_var: Option<String>,
    pub(crate) blue_queue_group_env_var: Option<String>,
    pub(crate) temporary_subject_identifier: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[allow(dead_code)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CombinerConfig {
    pub(crate) output_subject: Option<String>,
    pub(crate) green_output_subject: Option<String>,
    pub(crate) blue_output_subject: Option<String>,
    pub(crate) green_output_subject_env_var: Option<String>,
    pub(crate) blue_output_subject_env_var: Option<String>,
    pub(crate) temporary_subject_identifier: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WriterConfig {
    pub(crate) target_subject: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConsumerConfig {
    pub(crate) input_subject: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct WriteRequest {
    pub(crate) test_id: Option<String>,
    pub(crate) payload: Value,
}

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) config: Config,
    pub(crate) roles: Vec<PluginRole>,
    pub(crate) runtime: PluginInceptorRuntime,
    pub(crate) nats_url: String,
    mode: Arc<AtomicU8>,
    traffic_percent: Arc<AtomicU8>,
    core_ready_mask: Arc<AtomicU8>,
    core_ready_notify: Arc<Notify>,
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
        nats_url: String,
    ) -> Self {
        Self {
            config,
            roles,
            runtime,
            nats_url,
            mode: Arc::new(AtomicU8::new(RuntimeMode::Idle as u8)),
            traffic_percent: Arc::new(
                AtomicU8::new(fluidbg_plugin_sdk::traffic_percent_from_env()),
            ),
            core_ready_mask: Arc::new(AtomicU8::new(0)),
            core_ready_notify: Arc::new(Notify::new()),
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

    pub(crate) fn reset_core_readiness(&self) {
        self.core_ready_mask.store(0, Ordering::Relaxed);
    }

    pub(crate) fn mark_core_ready(&self, mask: u8) {
        self.core_ready_mask.fetch_or(mask, Ordering::Relaxed);
        self.core_ready_notify.notify_waiters();
    }

    pub(crate) fn core_ready(&self, expected: u8) -> bool {
        expected == 0 || (self.core_ready_mask.load(Ordering::Relaxed) & expected) == expected
    }

    pub(crate) async fn wait_for_core_ready(
        &self,
        expected: u8,
        timeout: std::time::Duration,
    ) -> bool {
        if self.core_ready(expected) {
            return true;
        }
        tokio::time::timeout(timeout, async {
            loop {
                self.core_ready_notify.notified().await;
                if self.core_ready(expected) {
                    break;
                }
            }
        })
        .await
        .is_ok()
    }
}

pub(crate) fn load_config() -> Result<Config> {
    fluidbg_plugin_sdk::load_yaml_config()
}

pub(crate) fn nats_url_from_env() -> Result<String> {
    std::env::var("FLUIDBG_NATS_URL").context(
        "missing FLUIDBG_NATS_URL; install the NATS plugin manager or provide runtime credentials",
    )
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

pub(crate) fn routes_to_blue(payload: &[u8], traffic_percent: u8) -> bool {
    fluidbg_plugin_sdk::routes_to_blue(payload, traffic_percent)
}

#[cfg(test)]
mod tests {
    use super::Config;
    use super::NatsMode;
    use serde_json::json;

    #[test]
    fn rollout_config_rejects_runtime_credentials() {
        let parsed = serde_json::from_value::<Config>(json!({
            "natsUrl": "nats://admin:admin@nats:4222",
            "splitter": {"inputSubject": "orders"}
        }));
        assert!(parsed.is_err());
    }

    #[test]
    fn subject_identifiers_are_role_local() {
        let parsed: Config = serde_json::from_value(json!({
            "splitter": {
                "inputSubject": "orders",
                "temporarySubjectIdentifier": "incoming-orders"
            }
        }))
        .unwrap();
        assert_eq!(
            parsed
                .splitter
                .unwrap()
                .temporary_subject_identifier
                .as_deref(),
            Some("incoming-orders")
        );
    }

    #[test]
    fn nats_mode_defaults_to_jetstream() {
        let parsed: Config = serde_json::from_value(json!({})).unwrap();
        assert_eq!(parsed.mode, NatsMode::JetStream);
    }

    #[test]
    fn nats_mode_accepts_core() {
        let parsed: Config = serde_json::from_value(json!({"mode": "core"})).unwrap();
        assert_eq!(parsed.mode, NatsMode::Core);
    }
}
