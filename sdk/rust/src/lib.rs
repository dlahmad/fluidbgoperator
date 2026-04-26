use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use anyhow::Result;
use chrono::Utc;
use regex::Regex;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

pub const PLUGIN_API_VERSION: &str = "fluidbg.plugin/v1alpha1";
pub const CRD_GROUP: &str = "fluidbg.io";
pub const CRD_VERSION: &str = "v1alpha1";
pub const SDK_VERSION: &str = env!("CARGO_PKG_VERSION");

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

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginLifecycleResponse {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assignments: Vec<PropertyAssignment>,
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

#[derive(Clone)]
pub struct PluginRuntime {
    client: reqwest::Client,
    roles: Vec<PluginRole>,
    mode: String,
    testcase_registration_url: String,
    test_container_url: String,
    testcase_verify_path_template: Option<String>,
    inception_point: String,
    blue_green_ref: String,
}

impl PluginRuntime {
    pub fn from_env() -> Self {
        Self {
            client: reqwest::Client::new(),
            roles: active_roles(),
            mode: std::env::var("FLUIDBG_MODE")
                .unwrap_or_else(|_| "passthrough-duplicate".to_string()),
            testcase_registration_url: std::env::var("FLUIDBG_TESTCASE_REGISTRATION_URL")
                .unwrap_or_else(|_| "http://localhost:8090/testcases".to_string()),
            test_container_url: std::env::var("FLUIDBG_TEST_CONTAINER_URL")
                .unwrap_or_else(|_| "http://localhost:8080".to_string()),
            testcase_verify_path_template: std::env::var("FLUIDBG_TESTCASE_VERIFY_PATH_TEMPLATE")
                .ok(),
            inception_point: std::env::var("FLUIDBG_INCEPTION_POINT")
                .unwrap_or_else(|_| "unknown".to_string()),
            blue_green_ref: std::env::var("FLUIDBG_BLUE_GREEN_REF")
                .unwrap_or_else(|_| "unknown".to_string()),
        }
    }

    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    pub fn roles(&self) -> &[PluginRole] {
        &self.roles
    }

    pub fn mode(&self) -> &str {
        &self.mode
    }

    pub fn test_container_url(&self) -> &str {
        &self.test_container_url
    }

    pub fn inception_point(&self) -> &str {
        &self.inception_point
    }

    pub fn blue_green_ref(&self) -> &str {
        &self.blue_green_ref
    }

    pub fn has_role(&self, role: PluginRole) -> bool {
        has_role(&self.roles, role)
    }

    pub fn observes(&self) -> bool {
        self.has_role(PluginRole::Observer)
            || self.mode == "trigger"
            || self.mode == "passthrough-duplicate"
    }

    pub fn mocks(&self) -> bool {
        self.has_role(PluginRole::Mock) || self.mode == "reroute-mock"
    }

    pub async fn register_test_case(&self, test_id: &str) -> Result<()> {
        register_test_case(
            &self.client,
            &self.testcase_registration_url,
            &self.blue_green_ref,
            &self.inception_point,
            test_id,
            &self.test_container_url,
            self.testcase_verify_path_template.as_deref(),
        )
        .await
    }

    pub async fn notify_observer(
        &self,
        notify_path: &str,
        test_id: &str,
        payload: &Value,
        route: TrafficRoute,
    ) -> Result<()> {
        notify_observer(
            &self.client,
            &self.test_container_url,
            notify_path,
            test_id,
            &self.inception_point,
            payload,
            route,
        )
        .await
    }
}

pub fn load_yaml_config<T: DeserializeOwned>() -> Result<T> {
    let path = std::env::var("FLUIDBG_CONFIG_PATH")
        .unwrap_or_else(|_| "/etc/fluidbg/config.yaml".to_string());
    let data = std::fs::read_to_string(path)?;
    Ok(serde_yaml_ng::from_str(&data)?)
}

pub fn active_roles() -> Vec<PluginRole> {
    std::env::var("FLUIDBG_ACTIVE_ROLES")
        .unwrap_or_default()
        .split(',')
        .filter_map(PluginRole::parse)
        .collect()
}

pub fn has_role(roles: &[PluginRole], role: PluginRole) -> bool {
    roles.contains(&role)
}

pub fn traffic_percent_from_env() -> u8 {
    std::env::var("FLUIDBG_TRAFFIC_PERCENT")
        .ok()
        .and_then(|value| value.parse::<u8>().ok())
        .unwrap_or(100)
        .min(100)
}

pub fn routes_to_blue(payload: &[u8], traffic_percent: u8) -> bool {
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

pub fn extract_json_path(value: &Value, path: &str) -> Option<String> {
    let stripped = path.strip_prefix('$').unwrap_or(path);
    let stripped = stripped.strip_prefix('.').unwrap_or(stripped);
    let mut current = value;
    for key in stripped.split('.') {
        current = current.as_object()?.get(key)?;
    }
    match current {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

pub fn condition_matches(value: Option<String>, condition: &FilterCondition) -> bool {
    let Some(value) = value else {
        return false;
    };
    if let Some(expected) = &condition.equals {
        return value == *expected;
    }
    if let Some(pattern) = &condition.matches {
        return Regex::new(pattern)
            .map(|regex| regex.is_match(&value))
            .unwrap_or(false);
    }
    true
}

pub fn match_conditions<'a, F>(conditions: &'a [FilterCondition], mut resolver: F) -> bool
where
    F: FnMut(&'a FilterCondition) -> Option<String>,
{
    if conditions.is_empty() {
        return true;
    }
    conditions
        .iter()
        .all(|condition| condition_matches(resolver(condition), condition))
}

pub fn resolve_http_field<F>(
    condition: &FilterCondition,
    method: &str,
    path: &str,
    body: &Value,
    mut header_lookup: F,
) -> Option<String>
where
    F: FnMut(&str) -> Option<String>,
{
    match condition.field.as_str() {
        "http.method" => Some(method.to_string()),
        "http.path" => Some(path.to_string()),
        "http.body" => {
            if let Some(json_path) = &condition.json_path {
                extract_json_path(body, json_path)
            } else {
                body.as_str().map(|value| value.to_string())
            }
        }
        field if field.starts_with("http.header.") => {
            let key = field.strip_prefix("http.header.")?;
            header_lookup(key)
        }
        field if field.starts_with("http.query.") => {
            let key = field.strip_prefix("http.query.")?;
            let query_str = path.split('?').nth(1).unwrap_or("");
            for pair in query_str.split('&') {
                let mut kv = pair.splitn(2, '=');
                if kv.next() == Some(key)
                    && let Some(value) = kv.next()
                {
                    return Some(value.to_string());
                }
            }
            None
        }
        _ => None,
    }
}

pub fn extract_http_test_id<F>(
    selector: &TestIdSelector,
    body: &Value,
    path: &str,
    mut header_lookup: F,
) -> Option<String>
where
    F: FnMut(&str) -> Option<String>,
{
    if let Some(value) = &selector.value {
        return Some(value.clone());
    }
    let field = selector.field.as_ref()?;
    match field.as_str() {
        "http.body" => {
            if let Some(json_path) = &selector.json_path {
                extract_json_path(body, json_path)
            } else {
                body.as_str().map(|value| value.to_string())
            }
        }
        "http.path" => {
            if let Some(segment) = selector.path_segment {
                path.split('/')
                    .filter(|part| !part.is_empty())
                    .nth((segment - 1) as usize)
                    .map(|part| part.to_string())
            } else {
                Some(path.to_string())
            }
        }
        field if field.starts_with("http.header.") => {
            let key = field.strip_prefix("http.header.")?;
            header_lookup(key)
        }
        _ => None,
    }
}

pub fn render_path(template: &str, test_id: &str, inception_point: &str) -> String {
    template
        .replace("{testId}", test_id)
        .replace("{inceptionPoint}", inception_point)
}

pub async fn notify_observer(
    client: &reqwest::Client,
    test_container_url: &str,
    notify_path: &str,
    test_id: &str,
    inception_point: &str,
    payload: &Value,
    route: TrafficRoute,
) -> Result<()> {
    let path = render_path(notify_path, test_id, inception_point);
    let notification = ObservationNotification {
        test_id,
        inception_point,
        route: route.as_str(),
        payload,
    };
    client
        .post(format!(
            "{}{}",
            test_container_url.trim_end_matches('/'),
            path
        ))
        .json(&notification)
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}

pub async fn register_test_case(
    client: &reqwest::Client,
    testcase_registration_url: &str,
    blue_green_ref: &str,
    inception_point: &str,
    test_id: &str,
    test_container_url: &str,
    testcase_verify_path_template: Option<&str>,
) -> Result<()> {
    let verify_url = testcase_verify_path_template.map(|path| {
        format!(
            "{}{}",
            test_container_url.trim_end_matches('/'),
            render_path(path, test_id, inception_point)
        )
    });
    let request = RegisterTestCaseRequest {
        test_id: test_id.to_string(),
        blue_green_ref: blue_green_ref.to_string(),
        inception_point: inception_point.to_string(),
        triggered_at: Some(Utc::now()),
        timeout_seconds: Some(120),
        verify_url,
    };
    client
        .post(testcase_registration_url)
        .json(&request)
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn json_path_extracts_scalar_values() {
        let value = json!({"order": {"id": 17, "ok": true}});
        assert_eq!(
            extract_json_path(&value, "$.order.id"),
            Some("17".to_string())
        );
        assert_eq!(
            extract_json_path(&value, "$.order.ok"),
            Some("true".to_string())
        );
        assert_eq!(extract_json_path(&value, "$.missing"), None);
    }

    #[test]
    fn route_registration_semantics_skip_green_only() {
        assert!(TrafficRoute::Blue.should_register_case());
        assert!(TrafficRoute::Both.should_register_case());
        assert!(TrafficRoute::Unknown.should_register_case());
        assert!(!TrafficRoute::Green.should_register_case());
    }

    #[test]
    fn weighted_routing_boundaries_are_stable() {
        assert!(!routes_to_blue(b"case", 0));
        assert!(routes_to_blue(b"case", 100));
        assert_eq!(routes_to_blue(b"case", 25), routes_to_blue(b"case", 25));
    }

    #[test]
    fn http_selector_extracts_without_route_payload_field() {
        let selector = TestIdSelector {
            field: Some("http.body".to_string()),
            json_path: Some("$.orderId".to_string()),
            path_segment: None,
            value: None,
        };
        let body = json!({"orderId": "order-17"});
        assert_eq!(
            extract_http_test_id(&selector, &body, "/orders", |_| None),
            Some("order-17".to_string())
        );
    }

    #[test]
    fn sdk_versions_match_manifest() {
        let manifest: Value =
            serde_yaml_ng::from_str(include_str!("../../spec/crd-versions.yaml")).unwrap();
        assert_eq!(manifest["pluginApiVersion"], PLUGIN_API_VERSION);
        assert_eq!(manifest["crds"]["group"], CRD_GROUP);
        assert_eq!(manifest["crds"]["version"], CRD_VERSION);
        assert_eq!(manifest["sdk"]["rustCrate"], "fluidbg-plugin-sdk");
        assert_eq!(manifest["sdk"]["version"], SDK_VERSION);
    }
}
