use std::collections::BTreeMap;

use fluidbg_plugin_sdk::{
    FilterCondition, ObserverConfig, TestIdSelector, TrafficRoute, condition_matches,
    extract_json_path,
};
use serde_json::Value;
use tracing::warn;

use crate::config::{AppState, CombinerConfig};

pub(crate) fn route_from_output_source(
    combiner: &CombinerConfig,
    source_queue: &str,
) -> TrafficRoute {
    if combiner
        .blue_output_queue
        .as_deref()
        .is_some_and(|queue| queue == source_queue)
    {
        return TrafficRoute::Blue;
    }
    if combiner
        .green_output_queue
        .as_deref()
        .is_some_and(|queue| queue == source_queue)
    {
        return TrafficRoute::Green;
    }
    TrafficRoute::Unknown
}

pub(crate) fn extract_test_id(
    selector: &TestIdSelector,
    body: &Value,
    properties: &BTreeMap<String, String>,
) -> Option<String> {
    if let Some(val) = &selector.value {
        return Some(val.clone());
    }
    let field = selector.field.as_ref()?;
    match field.as_str() {
        "queue.body" | "servicebus.body" => {
            if let Some(jp) = &selector.json_path {
                extract_json_path(body, jp)
            } else {
                body.as_str().map(str::to_string)
            }
        }
        f if f.starts_with("queue.property.") => {
            properties.get(f.strip_prefix("queue.property.")?).cloned()
        }
        f if f.starts_with("servicebus.property.") => properties
            .get(f.strip_prefix("servicebus.property.")?)
            .cloned(),
        _ => None,
    }
}

pub(crate) fn matches_filter(
    conditions: &[FilterCondition],
    body: &Value,
    properties: &BTreeMap<String, String>,
) -> bool {
    if conditions.is_empty() {
        return true;
    }
    conditions.iter().all(|c| {
        let value = match c.field.as_str() {
            "queue.body" | "servicebus.body" => {
                if let Some(jp) = &c.json_path {
                    extract_json_path(body, jp)
                } else {
                    body.as_str().map(str::to_string)
                }
            }
            f if f.starts_with("queue.property.") => properties
                .get(f.strip_prefix("queue.property.").unwrap_or(""))
                .cloned(),
            f if f.starts_with("servicebus.property.") => properties
                .get(f.strip_prefix("servicebus.property.").unwrap_or(""))
                .cloned(),
            _ => None,
        };
        let value = match value {
            Some(v) => v,
            None => return false,
        };
        condition_matches(Some(value), c)
    })
}

pub(crate) async fn notify_observer(
    state: &AppState,
    observer: &ObserverConfig,
    test_id: &str,
    payload: &Value,
    route: TrafficRoute,
) {
    if let Some(path) = observer.notify_path.as_deref()
        && let Err(err) = state
            .runtime
            .notify_observer(path, test_id, payload, route)
            .await
    {
        warn!("failed to notify test container for {}: {}", test_id, err);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use fluidbg_plugin_sdk::TestIdSelector;
    use serde_json::json;

    use super::extract_test_id;

    #[test]
    fn extracts_test_id_from_service_bus_property_without_payload_contract() {
        let mut properties = BTreeMap::new();
        properties.insert("x-test-id".to_string(), "case-17".to_string());
        let selector = TestIdSelector {
            field: Some("queue.property.x-test-id".to_string()),
            json_path: None,
            path_segment: None,
            value: None,
        };

        assert_eq!(
            extract_test_id(&selector, &json!({"ignored": true}), &properties),
            Some("case-17".to_string())
        );
    }
}
