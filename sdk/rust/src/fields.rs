use regex::Regex;
use serde_json::Value;

use crate::models::FilterCondition;

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
