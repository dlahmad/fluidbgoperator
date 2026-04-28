use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginRole {
    Duplicator,
    Splitter,
    Combiner,
    Observer,
    Mock,
    Writer,
    Consumer,
}

impl PluginRole {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "duplicator" => Some(Self::Duplicator),
            "splitter" => Some(Self::Splitter),
            "combiner" => Some(Self::Combiner),
            "observer" => Some(Self::Observer),
            "mock" => Some(Self::Mock),
            "writer" => Some(Self::Writer),
            "consumer" => Some(Self::Consumer),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrafficRoute {
    Blue,
    Green,
    Both,
    Unknown,
}

impl TrafficRoute {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Blue => "blue",
            Self::Green => "green",
            Self::Both => "both",
            Self::Unknown => "unknown",
        }
    }

    pub fn should_register_case(self) -> bool {
        !matches!(self, Self::Green)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
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

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FilterCondition {
    pub field: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equals: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matches: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json_path: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObserverConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_id: Option<TestIdSelector>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub r#match: Vec<FilterCondition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify_path: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PayloadSelection {
    Request,
    Response,
    Both,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationFilter {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub r#match: Vec<FilterCondition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<PayloadSelection>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AssignmentTarget {
    Green,
    Blue,
    Test,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AssignmentKind {
    Env,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PropertyAssignment {
    pub target: AssignmentTarget,
    pub kind: AssignmentKind,
    pub name: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_name: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InceptorEnvVar {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginLifecycleResponse {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assignments: Vec<PropertyAssignment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inceptor_env: Vec<InceptorEnvVar>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginDrainStatusResponse {
    pub drained: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrafficShiftRequest {
    pub traffic_percent: u8,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrafficShiftResponse {
    pub traffic_percent: u8,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveInception {
    pub namespace: String,
    pub blue_green_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blue_green_uid: Option<String>,
    pub inception_point: String,
    pub plugin: String,
    pub roles: Vec<String>,
    pub config: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginManagerLifecycleRequest {
    pub namespace: String,
    pub blue_green_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blue_green_uid: Option<String>,
    pub inception_point: String,
    pub plugin: String,
    pub roles: Vec<String>,
    pub config: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_inceptions: Vec<ActiveInception>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginManagerSyncRequest {
    pub plugin: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_inceptions: Vec<ActiveInception>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservationNotification<'a> {
    pub test_id: &'a str,
    pub inception_point: &'a str,
    pub route: &'a str,
    pub payload: &'a Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct RegisterTestCaseRequest {
    pub test_id: String,
    pub blue_green_ref: String,
    pub inception_point: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triggered_at: Option<chrono::DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify_url: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpWriteRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_id: Option<String>,
    pub payload: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}
