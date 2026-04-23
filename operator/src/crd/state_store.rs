use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub enum StateStoreType {
    #[serde(rename = "memory")]
    Memory,
    #[serde(rename = "redis")]
    Redis,
    #[serde(rename = "postgres")]
    Postgres,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PostgresConfig {
    pub url: String,
    #[serde(default = "default_table_name")]
    pub table_name: String,
    #[serde(default = "default_ttl_seconds")]
    pub ttl_seconds: i64,
}

fn default_table_name() -> String {
    "fluidbg_cases".to_string()
}

fn default_ttl_seconds() -> i64 {
    86400
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RedisConfig {
    pub url: String,
    #[serde(default = "default_ttl_seconds")]
    pub ttl_seconds: i64,
}

#[derive(Clone, Debug, CustomResource, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "fluidbg.io",
    version = "v1alpha1",
    kind = "StateStore",
    namespaced
)]
pub struct StateStoreSpec {
    #[serde(rename = "type")]
    pub store_type: StateStoreType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub postgres: Option<PostgresConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redis: Option<RedisConfig>,
}
