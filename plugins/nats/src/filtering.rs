use fluidbg_plugin_sdk::{
    FilterCondition, ObserverConfig, TestIdSelector, TrafficRoute, extract_json_path,
};
use serde_json::Value;

use crate::config::{AppState, CombinerConfig};

pub(crate) fn matches_filter(conditions: &[FilterCondition], body: &Value) -> bool {
    conditions.iter().all(|condition| {
        fluidbg_plugin_sdk::condition_matches(resolve_field(condition, body), condition)
    })
}

pub(crate) fn extract_test_id(selector: &TestIdSelector, body: &Value) -> Option<String> {
    if let Some(value) = &selector.value {
        return Some(value.clone());
    }
    match selector.field.as_deref() {
        Some("nats.body") | Some("queue.body") => selector
            .json_path
            .as_deref()
            .and_then(|path| fluidbg_plugin_sdk::extract_json_path(body, path)),
        _ => None,
    }
}

pub(crate) async fn notify_observer(
    state: &AppState,
    observer: &ObserverConfig,
    test_id: &str,
    body: &Value,
    route: TrafficRoute,
) -> bool {
    let Some(path) = &observer.notify_path else {
        return false;
    };
    match state
        .runtime
        .notify_observer(path, test_id, body, route)
        .await
    {
        Ok(()) => true,
        Err(err) => {
            tracing::warn!("failed to notify verifier for {}: {}", test_id, err);
            false
        }
    }
}

pub(crate) fn route_from_output_source(config: &CombinerConfig, subject: &str) -> TrafficRoute {
    if config.green_output_subject.as_deref() == Some(subject) {
        TrafficRoute::Green
    } else if config.blue_output_subject.as_deref() == Some(subject) {
        TrafficRoute::Blue
    } else {
        TrafficRoute::Unknown
    }
}

fn resolve_field(condition: &FilterCondition, body: &Value) -> Option<String> {
    match condition.field.as_str() {
        "nats.body" | "queue.body" => condition
            .json_path
            .as_deref()
            .and_then(|path| extract_json_path(body, path))
            .or_else(|| serde_json::to_string(body).ok()),
        field => field
            .strip_prefix("nats.body.")
            .and_then(|path| fluidbg_plugin_sdk::extract_json_path(body, path)),
    }
}

#[cfg(test)]
mod tests {
    use fluidbg_plugin_sdk::{FilterCondition, TestIdSelector, TrafficRoute};
    use serde_json::json;

    use super::{extract_test_id, matches_filter, route_from_output_source};
    use crate::config::CombinerConfig;

    #[test]
    fn matches_nats_body_filter_with_json_path() {
        let body = json!({"orderId": "case-1", "type": "order"});
        let conditions = vec![FilterCondition {
            field: "nats.body".to_string(),
            equals: None,
            matches: Some("^order$".to_string()),
            json_path: Some("$.type".to_string()),
        }];

        assert!(matches_filter(&conditions, &body));
    }

    #[test]
    fn extracts_test_id_from_nats_body_json_path() {
        let body = json!({"orderId": "case-1"});
        let selector = TestIdSelector {
            field: Some("nats.body".to_string()),
            json_path: Some("$.orderId".to_string()),
            path_segment: None,
            value: None,
        };

        assert_eq!(extract_test_id(&selector, &body).as_deref(), Some("case-1"));
    }

    #[test]
    fn resolves_route_from_combiner_source_subject() {
        let config = CombinerConfig {
            output_subject: Some("results".to_string()),
            green_output_subject: Some("results-green".to_string()),
            blue_output_subject: Some("results-blue".to_string()),
            green_output_subject_env_var: None,
            blue_output_subject_env_var: None,
            temporary_subject_identifier: None,
        };

        assert_eq!(
            route_from_output_source(&config, "results-green"),
            TrafficRoute::Green
        );
        assert_eq!(
            route_from_output_source(&config, "results-blue"),
            TrafficRoute::Blue
        );
    }
}
