use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Config {
    #[serde(default)]
    pub(crate) amqp_url: Option<String>,
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

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ObserverConfig {
    #[serde(default)]
    pub(crate) test_id: Option<TestIdSelector>,
    #[serde(default)]
    pub(crate) r#match: Vec<FilterCondition>,
    #[serde(default)]
    pub(crate) notify_path: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ActiveRole {
    Duplicator,
    Splitter,
    Combiner,
    Observer,
    Writer,
    Consumer,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TestIdSelector {
    pub(crate) field: Option<String>,
    pub(crate) json_path: Option<String>,
    pub(crate) value: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FilterCondition {
    pub(crate) field: String,
    #[serde(default)]
    pub(crate) equals: Option<String>,
    #[serde(default)]
    pub(crate) matches: Option<String>,
    #[serde(default)]
    pub(crate) json_path: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum AssignmentTarget {
    Green,
    Blue,
    Test,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum AssignmentKind {
    Env,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PropertyAssignment {
    pub(crate) target: AssignmentTarget,
    pub(crate) kind: AssignmentKind,
    pub(crate) name: String,
    pub(crate) value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) container_name: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PluginLifecycleResponse {
    #[serde(default)]
    pub(crate) assignments: Vec<PropertyAssignment>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PluginDrainStatusResponse {
    pub(crate) drained: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) message: Option<String>,
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
    pub(crate) roles: Vec<ActiveRole>,
    pub(crate) amqp_url: String,
    pub(crate) testcase_registration_url: String,
    pub(crate) test_container_url: String,
    pub(crate) testcase_verify_path_template: Option<String>,
    pub(crate) inception_point: String,
    pub(crate) blue_green_ref: String,
    mode: Arc<AtomicU8>,
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
        roles: Vec<ActiveRole>,
        amqp_url: String,
        testcase_registration_url: String,
        test_container_url: String,
        testcase_verify_path_template: Option<String>,
        inception_point: String,
        blue_green_ref: String,
    ) -> Self {
        Self {
            config,
            roles,
            amqp_url,
            testcase_registration_url,
            test_container_url,
            testcase_verify_path_template,
            inception_point,
            blue_green_ref,
            mode: Arc::new(AtomicU8::new(RuntimeMode::Active as u8)),
        }
    }

    pub(crate) fn runtime_mode(&self) -> RuntimeMode {
        RuntimeMode::from_u8(self.mode.load(Ordering::Relaxed))
    }

    pub(crate) fn set_runtime_mode(&self, mode: RuntimeMode) {
        self.mode.store(mode as u8, Ordering::Relaxed);
    }
}

pub(crate) fn load_config() -> Result<Config> {
    let path = std::env::var("FLUIDBG_CONFIG_PATH")
        .unwrap_or_else(|_| "/etc/fluidbg/config.yaml".to_string());
    let data = std::fs::read_to_string(&path)?;
    Ok(serde_yaml_ng::from_str(&data)?)
}

pub(crate) fn load_roles() -> Vec<ActiveRole> {
    std::env::var("FLUIDBG_ACTIVE_ROLES")
        .unwrap_or_default()
        .split(',')
        .filter_map(|value| match value.trim() {
            "duplicator" => Some(ActiveRole::Duplicator),
            "splitter" => Some(ActiveRole::Splitter),
            "combiner" => Some(ActiveRole::Combiner),
            "observer" => Some(ActiveRole::Observer),
            "writer" => Some(ActiveRole::Writer),
            "consumer" => Some(ActiveRole::Consumer),
            _ => None,
        })
        .collect()
}

pub(crate) fn has_role(roles: &[ActiveRole], role: ActiveRole) -> bool {
    roles.contains(&role)
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
    std::env::var("FLUIDBG_TRAFFIC_PERCENT")
        .ok()
        .and_then(|value| value.parse::<u8>().ok())
        .unwrap_or(100)
        .min(100)
}

pub(crate) fn routes_to_blue(payload: &[u8], traffic_percent: u8) -> bool {
    if traffic_percent == 0 {
        return false;
    }
    if traffic_percent >= 100 {
        return true;
    }
    let mut hasher = DefaultHasher::new();
    payload.hash(&mut hasher);
    (hasher.finish() % 100) < traffic_percent as u64
}
