use fluidbg_plugin_sdk::{FilterCondition, ObserverConfig, TestIdSelector, TrafficRoute};
use serde_json::Value;

use crate::config::{AppState, CombinerConfig};

pub(crate) fn matches_filter(conditions: &[FilterCondition], body: &Value) -> bool {
    conditions.iter().all(|condition| {
        fluidbg_plugin_sdk::condition_matches(resolve_field(&condition.field, body), condition)
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

fn resolve_field(field: &str, body: &Value) -> Option<String> {
    match field {
        "nats.body" | "queue.body" => serde_json::to_string(body).ok(),
        _ => field
            .strip_prefix("nats.body.")
            .and_then(|path| fluidbg_plugin_sdk::extract_json_path(body, path)),
    }
}
