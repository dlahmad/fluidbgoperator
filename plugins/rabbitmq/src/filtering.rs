use fluidbg_plugin_sdk::{
    FilterCondition, ObserverConfig, TestIdSelector, TrafficRoute, condition_matches,
    extract_json_path,
};
use lapin::types::{AMQPValue, FieldTable};
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

fn amqp_value_as_string(v: &AMQPValue) -> Option<String> {
    match v {
        AMQPValue::LongString(s) => Some(s.to_string()),
        AMQPValue::ShortString(s) => Some(s.as_str().to_string()),
        _ => None,
    }
}

pub(crate) fn extract_test_id(
    selector: &TestIdSelector,
    body: &Value,
    properties: &FieldTable,
) -> Option<String> {
    if let Some(val) = &selector.value {
        return Some(val.clone());
    }
    let field = selector.field.as_ref()?;
    match field.as_str() {
        "queue.body" => {
            if let Some(jp) = &selector.json_path {
                extract_json_path(body, jp)
            } else {
                body.as_str().map(|s| s.to_string())
            }
        }
        f if f.starts_with("queue.property.") => {
            let key = f.strip_prefix("queue.property.")?;
            properties.inner().get(key).and_then(amqp_value_as_string)
        }
        _ => None,
    }
}

pub(crate) fn matches_filter(
    conditions: &[FilterCondition],
    body: &Value,
    properties: &FieldTable,
) -> bool {
    if conditions.is_empty() {
        return true;
    }
    conditions.iter().all(|c| {
        let value = match c.field.as_str() {
            "queue.body" => {
                if let Some(jp) = &c.json_path {
                    extract_json_path(body, jp)
                } else {
                    body.as_str().map(|s| s.to_string())
                }
            }
            f if f.starts_with("queue.property.") => {
                let key = f.strip_prefix("queue.property.").unwrap_or("");
                properties.inner().get(key).and_then(amqp_value_as_string)
            }
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
) -> bool {
    let Some(path) = observer.notify_path.as_deref() else {
        warn!(
            "observer matched test case {} but no notifyPath is configured",
            test_id
        );
        return false;
    };
    if let Err(err) = state
        .runtime
        .notify_observer(path, test_id, payload, route)
        .await
    {
        warn!("failed to notify test container for {}: {}", test_id, err);
        return false;
    }
    true
}
