use serde_json::Value;

use crate::fields::extract_json_path;
use crate::models::{FilterCondition, TestIdSelector};

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
    let (path_only, query_str) = split_path_query(path);
    match condition.field.as_str() {
        "http.method" => Some(method.to_string()),
        "http.path" => Some(path_only.to_string()),
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
    let (path_only, _) = split_path_query(path);
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
                path_only
                    .split('/')
                    .filter(|part| !part.is_empty())
                    .nth((segment - 1) as usize)
                    .map(|part| part.to_string())
            } else {
                Some(path_only.to_string())
            }
        }
        field if field.starts_with("http.header.") => {
            let key = field.strip_prefix("http.header.")?;
            header_lookup(key)
        }
        _ => None,
    }
}

fn split_path_query(path_and_query: &str) -> (&str, &str) {
    path_and_query
        .split_once('?')
        .unwrap_or((path_and_query, ""))
}
