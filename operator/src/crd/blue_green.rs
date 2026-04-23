use k8s_openapi::api::apps::v1::Deployment;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::inception_plugin::PluginRole;

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct DeploymentRef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct BlueSpec {
    pub deployment: DeploymentRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "arbitrary_object_schema")]
    pub template: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest: Option<Deployment>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct GreenSpec {
    pub selector: DeploymentSelector,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentSelector {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    pub match_labels: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct PluginRef {
    pub name: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FilterMatch {
    pub field: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equals: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matches: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json_path: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum PayloadOption {
    Request,
    Response,
    Both,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Filter {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub r#match: Vec<FilterMatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<PayloadOption>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TestIdSelector {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_segment: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InceptionPoint {
    pub name: String,
    pub plugin_ref: PluginRef,
    pub roles: Vec<PluginRole>,
    #[schemars(schema_with = "arbitrary_object_schema")]
    pub config: serde_json::Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notify_tests: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TestSpec {
    pub name: String,
    pub image: String,
    pub port: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_path: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<TestEnvVar>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct TestEnvVar {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SuccessCriteria {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_cases: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub success_rate: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation_window_minutes: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProgressiveStep {
    pub traffic_percent: i32,
    pub observe_cases: i64,
    pub success_rate: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProgressiveStrategy {
    pub steps: Vec<ProgressiveStep>,
    #[serde(default = "default_true")]
    pub rollback_on_step_failure: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_timeout_minutes: Option<i64>,
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum StrategyType {
    HardSwitch,
    Progressive,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct PromotionStrategy {
    #[serde(rename = "type")]
    pub strategy_type: StrategyType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progressive: Option<ProgressiveStrategy>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PromotionSpec {
    pub success_criteria: SuccessCriteria,
    pub strategy: PromotionStrategy,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "PascalCase")]
pub enum BGDPhase {
    Pending,
    Observing,
    Promoting,
    Completed,
    RolledBack,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BlueGreenDeploymentStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<BGDPhase>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cases_observed: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cases_passed: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cases_failed: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cases_pending: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cases_timed_out: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_success_rate: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_traffic_percent: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_step: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_case_at: Option<String>,
}

#[derive(Clone, Debug, CustomResource, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[kube(
    group = "fluidbg.io",
    version = "v1alpha1",
    kind = "BlueGreenDeployment",
    namespaced
)]
#[kube(status = "BlueGreenDeploymentStatus")]
pub struct BlueGreenDeploymentSpec {
    pub state_store_ref: PluginRef,
    pub green: GreenSpec,
    pub blue: BlueSpec,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inception_points: Vec<InceptionPoint>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tests: Vec<TestSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promotion: Option<PromotionSpec>,
}

fn arbitrary_object_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "object",
        "x-kubernetes-preserve-unknown-fields": true
    })
}
