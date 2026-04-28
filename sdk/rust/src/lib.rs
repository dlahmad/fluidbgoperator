mod auth;
mod config;
mod fields;
mod http;
mod models;
mod notify;
mod resources;
mod routing;
mod runtime;
mod version;

pub use auth::{
    AUTHORIZATION_HEADER, PLUGIN_AUTH_AUDIENCE, PLUGIN_AUTH_ISSUER, PLUGIN_AUTH_TOKEN_ENV,
    PluginAuthClaims, auth_token_from_env, bearer_matches, bearer_token, bearer_value,
    sign_plugin_auth_token, verify_plugin_auth_token,
};
pub use config::{active_roles, has_role, load_yaml_config, traffic_percent_from_env};
pub use fields::{condition_matches, extract_json_path, match_conditions};
pub use http::{extract_http_test_id, resolve_http_field};
pub use models::{
    AssignmentKind, AssignmentTarget, FilterCondition, HttpWriteRequest, NotificationFilter,
    ObservationNotification, ObserverConfig, PayloadSelection, PluginDrainStatusResponse,
    PluginLifecycleResponse, PluginManagerLifecycleRequest, PluginRole, PropertyAssignment,
    RegisterTestCaseRequest, TestIdSelector, TrafficRoute, TrafficShiftRequest,
    TrafficShiftResponse,
};
pub use notify::{RegisterTestCaseArgs, notify_observer, register_test_case, render_path};
pub use resources::{
    derived_shadow_queue_name, derived_temp_queue_name, derived_temp_queue_name_with_uid,
};
pub use routing::routes_to_blue;
pub use runtime::{PluginInceptorRuntime, PluginRuntime};
pub use version::{CRD_GROUP, CRD_VERSION, PLUGIN_API_VERSION, SDK_VERSION};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

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
