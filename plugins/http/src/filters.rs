use axum::http::HeaderMap;
use fluidbg_plugin_sdk::{
    FilterCondition, NotificationFilter, TestIdSelector, extract_http_test_id, match_conditions,
    resolve_http_field,
};
use serde_json::Value;

use crate::config::Config;

pub(crate) fn matches_filter(
    conditions: &[FilterCondition],
    method: &str,
    path: &str,
    headers: &HeaderMap,
    body: &Value,
) -> bool {
    match_conditions(conditions, |condition| {
        resolve_field(condition, method, path, headers, body)
    })
}

fn resolve_field(
    c: &FilterCondition,
    method: &str,
    path: &str,
    headers: &HeaderMap,
    body: &Value,
) -> Option<String> {
    resolve_http_field(c, method, path, body, |key| {
        headers
            .get(key)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.to_string())
    })
}

pub(crate) fn matching_filter<'a>(
    config: &'a Config,
    method: &str,
    path: &str,
    headers: &HeaderMap,
    body: &Value,
) -> Option<&'a NotificationFilter> {
    let direction_filters = config
        .ingress
        .as_ref()
        .into_iter()
        .chain(config.egress.as_ref())
        .flat_map(|direction| direction.filters.iter());
    config
        .filters
        .iter()
        .chain(direction_filters)
        .find(|filter| matches_filter(&filter.r#match, method, path, headers, body))
}

pub(crate) fn has_any_filters(config: &Config) -> bool {
    !config.filters.is_empty()
        || config
            .ingress
            .as_ref()
            .is_some_and(|direction| !direction.filters.is_empty())
        || config
            .egress
            .as_ref()
            .is_some_and(|direction| !direction.filters.is_empty())
}

pub(crate) fn extract_test_id(
    sel: &TestIdSelector,
    body: &Value,
    path: &str,
    headers: &HeaderMap,
) -> Option<String> {
    extract_http_test_id(sel, body, path, |key| {
        headers
            .get(key)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.to_string())
    })
}
