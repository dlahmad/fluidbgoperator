use k8s_openapi::api::apps::v1::{DeploymentSpec, DeploymentStrategy};
use k8s_openapi::api::core::v1::PodTemplateSpec;
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
#[serde(rename_all = "camelCase")]
pub struct ManagedDeploymentSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    pub spec: DeploymentSpec,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TestDeploymentPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replicas: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<DeploymentStrategy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_ready_seconds: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision_history_limit: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress_deadline_seconds: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<PodTemplateSpec>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drain: Option<InceptionPointDrainSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(schema_with = "arbitrary_array_schema")]
    pub resources: Vec<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InceptionPointDrainSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_wait_seconds: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TestSpec {
    pub name: String,
    pub image: String,
    pub port: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_verification: Option<DataVerificationSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_verification: Option<CustomVerificationSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<TestEnvVar>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DataVerificationSpec {
    pub verify_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CustomVerificationSpec {
    pub start_path: String,
    pub verify_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retries: Option<i32>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct TestEnvVar {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DataPromotionSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_test_cases: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub success_rate: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CustomPromotionSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retries: Option<i32>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<DataPromotionSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom: Option<CustomPromotionSpec>,
    pub strategy: PromotionStrategy,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "PascalCase")]
pub enum BGDPhase {
    Pending,
    Observing,
    Promoting,
    Draining,
    Completed,
    RolledBack,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum InceptionPointDrainPhase {
    Pending,
    Successful,
    TimedOutMaybeSuccessful,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InceptionPointDrainStatus {
    pub name: String,
    pub phase: InceptionPointDrainPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq)]
#[serde(rename_all = "PascalCase")]
pub enum ConditionStatus {
    True,
    False,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BlueGreenDeploymentCondition {
    #[serde(rename = "type")]
    pub condition_type: String,
    pub status: ConditionStatus,
    pub reason: String,
    pub message: String,
    pub observed_generation: i64,
    pub last_transition_time: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BlueGreenDeploymentStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<BGDPhase>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_deployment_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollout_generation: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_cases_observed: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_cases_passed: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_cases_failed: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_cases_pending: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_cases_timed_out: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_success_rate: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_failure_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_traffic_percent: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_step: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_case_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drain_started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inception_point_drains: Vec<InceptionPointDrainStatus>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<BlueGreenDeploymentCondition>,
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
    pub selector: DeploymentSelector,
    pub deployment: ManagedDeploymentSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_deployment_patch: Option<TestDeploymentPatch>,
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

fn arbitrary_array_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "array",
        "items": {
            "type": "object",
            "x-kubernetes-preserve-unknown-fields": true
        }
    })
}

#[cfg(test)]
mod tests {
    use super::BlueGreenDeployment;

    #[test]
    fn deserialize_bootstrap_example() {
        let value = serde_json::json!({
            "apiVersion": "fluidbg.io/v1alpha1",
            "kind": "BlueGreenDeployment",
            "metadata": {
                "name": "order-processor-bootstrap",
                "namespace": "fluidbg-test"
            },
            "spec": {
                "selector": {
                    "matchLabels": {
                        "fluidbg.io/name": "order-processor"
                    }
                },
                "deployment": {
                    "spec": {
                        "replicas": 1,
                        "selector": {
                            "matchLabels": {
                                "app": "order-processor-green"
                            }
                        },
                        "template": {
                            "metadata": {
                                "labels": {
                                    "app": "order-processor-green",
                                    "fluidbg.io/name": "order-processor"
                                }
                            },
                            "spec": {
                                "terminationGracePeriodSeconds": 1,
                                "containers": [{
                                    "name": "order-processor",
                                    "image": "fluidbg/green-app:dev",
                                    "imagePullPolicy": "Never",
                                    "env": [
                                        { "name": "AMQP_URL", "value": "amqp://fluidbg:fluidbg@rabbitmq.fluidbg-system:5672/" },
                                        { "name": "INPUT_QUEUE", "value": "orders" },
                                        { "name": "OUTPUT_QUEUE", "value": "results" }
                                    ]
                                }]
                            }
                        }
                    }
                },
                "inceptionPoints": [{
                    "name": "incoming-orders",
                    "pluginRef": { "name": "rabbitmq" },
                    "roles": ["duplicator", "observer"],
                    "config": {
                        "amqpUrl": "amqp://fluidbg:fluidbg@rabbitmq.fluidbg-system:5672/%2f"
                    }
                }],
                "tests": [{
                    "name": "test-container",
                    "image": "fluidbg/test-app:dev",
                    "port": 8080,
                    "dataVerification": {
                        "verifyPath": "/result/{testId}",
                        "timeoutSeconds": 120
                    }
                }],
                "promotion": {
                    "data": {
                        "minTestCases": 1,
                        "successRate": 1.0,
                        "timeoutSeconds": 120
                    },
                    "strategy": {
                        "type": "hard-switch"
                    }
                }
            }
        });

        let parsed = serde_json::from_value::<BlueGreenDeployment>(value);
        assert!(parsed.is_ok(), "{parsed:?}");
    }
}
