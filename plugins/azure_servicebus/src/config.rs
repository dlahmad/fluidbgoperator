use std::sync::{
    Arc,
    atomic::{AtomicU8, AtomicUsize, Ordering},
};

use anyhow::{Context, Result};
use fluidbg_plugin_sdk::{ObserverConfig, PluginInceptorRuntime, PluginRole};
use serde::Deserialize;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
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

#[derive(Clone, Debug, Default, serde::Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct QueueDeclarationConfig {
    #[serde(default)]
    pub(crate) lock_duration: Option<String>,
    #[serde(default)]
    pub(crate) max_size_in_megabytes: Option<i64>,
    #[serde(default)]
    pub(crate) max_message_size_in_kilobytes: Option<i64>,
    #[serde(default)]
    pub(crate) requires_duplicate_detection: Option<bool>,
    #[serde(default)]
    pub(crate) requires_session: Option<bool>,
    #[serde(default)]
    pub(crate) default_message_time_to_live: Option<String>,
    #[serde(default)]
    pub(crate) dead_lettering_on_message_expiration: Option<bool>,
    #[serde(default)]
    pub(crate) duplicate_detection_history_time_window: Option<String>,
    #[serde(default)]
    pub(crate) max_delivery_count: Option<i32>,
    #[serde(default)]
    pub(crate) enable_batched_operations: Option<bool>,
    #[serde(default)]
    pub(crate) auto_delete_on_idle: Option<String>,
    #[serde(default)]
    pub(crate) enable_partitioning: Option<bool>,
    #[serde(default)]
    pub(crate) enable_express: Option<bool>,
    #[serde(default)]
    pub(crate) forward_to: Option<String>,
    #[serde(default)]
    pub(crate) forward_dead_lettered_messages_to: Option<String>,
    #[serde(default)]
    pub(crate) status: Option<String>,
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
    in_flight_messages: Arc<AtomicUsize>,
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
            mode: Arc::new(AtomicU8::new(RuntimeMode::Idle as u8)),
            traffic_percent: Arc::new(AtomicU8::new(blue_traffic_percent())),
            in_flight_messages: Arc::new(AtomicUsize::new(0)),
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

    pub(crate) fn track_message(&self) -> InFlightMessageGuard {
        self.in_flight_messages.fetch_add(1, Ordering::SeqCst);
        InFlightMessageGuard {
            in_flight_messages: self.in_flight_messages.clone(),
        }
    }

    pub(crate) fn in_flight_messages(&self) -> usize {
        self.in_flight_messages.load(Ordering::SeqCst)
    }
}

pub(crate) struct InFlightMessageGuard {
    in_flight_messages: Arc<AtomicUsize>,
}

impl Drop for InFlightMessageGuard {
    fn drop(&mut self) {
        self.in_flight_messages.fetch_sub(1, Ordering::SeqCst);
    }
}

pub(crate) fn load_config() -> Result<Config> {
    fluidbg_plugin_sdk::load_yaml_config()
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ServiceBusRuntimeConfig {
    pub(crate) connection_string: Option<String>,
    pub(crate) fully_qualified_namespace: Option<String>,
    pub(crate) auth: Option<AuthConfig>,
    pub(crate) management: Option<ManagementConfig>,
    pub(crate) static_sas_tokens: std::collections::BTreeMap<String, String>,
}

impl ServiceBusRuntimeConfig {
    pub(crate) fn from_env() -> Self {
        let auth_mode = std::env::var("FLUIDBG_AZURE_SERVICEBUS_AUTH_MODE")
            .ok()
            .and_then(|mode| match mode.as_str() {
                "connectionString" | "connection-string" | "connection_string" => {
                    Some(AuthMode::ConnectionString)
                }
                "workloadIdentity" | "workload-identity" | "workload_identity" => {
                    Some(AuthMode::WorkloadIdentity)
                }
                _ => None,
            });
        let auth = AuthConfig {
            mode: auth_mode,
            tenant_id: std::env::var("AZURE_TENANT_ID").ok(),
            client_id: std::env::var("AZURE_CLIENT_ID").ok(),
            federated_token_file: std::env::var("AZURE_FEDERATED_TOKEN_FILE").ok(),
            authority_host: std::env::var("AZURE_AUTHORITY_HOST").ok(),
            sas_token_ttl_seconds: std::env::var("FLUIDBG_AZURE_SERVICEBUS_SAS_TOKEN_TTL_SECONDS")
                .ok()
                .and_then(|value| value.parse().ok()),
        };
        let management = ManagementConfig {
            subscription_id: std::env::var("FLUIDBG_AZURE_SERVICEBUS_SUBSCRIPTION_ID").ok(),
            resource_group: std::env::var("FLUIDBG_AZURE_SERVICEBUS_RESOURCE_GROUP").ok(),
            namespace_name: std::env::var("FLUIDBG_AZURE_SERVICEBUS_NAMESPACE_NAME").ok(),
            create_queues: env_bool("FLUIDBG_AZURE_SERVICEBUS_CREATE_QUEUES", true),
            delete_queues: env_bool("FLUIDBG_AZURE_SERVICEBUS_DELETE_QUEUES", true),
        };
        Self {
            connection_string: std::env::var("AZURE_SERVICEBUS_CONNECTION_STRING").ok(),
            fully_qualified_namespace: std::env::var("AZURE_SERVICEBUS_NAMESPACE").ok(),
            auth: Some(auth),
            management: Some(management),
            static_sas_tokens: std::env::var("FLUIDBG_AZURE_SERVICEBUS_SAS_TOKENS_JSON")
                .ok()
                .and_then(|value| serde_json::from_str(&value).ok())
                .unwrap_or_default(),
        }
    }
}

fn env_bool(name: &str, default: bool) -> bool {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
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

fn default_create_queues() -> bool {
    true
}

fn default_delete_queues() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::Config;
    use serde_json::json;

    #[test]
    fn rollout_config_rejects_runtime_credentials() {
        let parsed = serde_json::from_value::<Config>(json!({
            "connectionString": "Endpoint=sb://example.servicebus.windows.net/;SharedAccessKeyName=RootManageSharedAccessKey;SharedAccessKey=secret",
            "fullyQualifiedNamespace": "example.servicebus.windows.net",
            "auth": {
                "mode": "connectionString"
            },
            "management": {
                "subscriptionId": "sub",
                "resourceGroup": "rg"
            },
            "duplicator": {
                "inputQueue": "orders"
            }
        }));

        assert!(parsed.is_err());
    }
}
