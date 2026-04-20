use regex::Regex;
use serde_json::Value;

use crate::plugins::fields;

#[derive(Clone, Debug)]
pub enum MatchOp {
    Equals(String),
    Matches(Regex),
}

#[derive(Clone, Debug)]
pub struct FilterCondition {
    pub field: String,
    pub json_path: Option<String>,
    pub op: MatchOp,
}

#[derive(Clone, Debug)]
pub struct FilterSet {
    pub conditions: Vec<FilterCondition>,
}

impl FilterSet {
    pub fn matches(&self, ctx: &FilterContext) -> bool {
        self.conditions.iter().all(|c| c.matches(ctx))
    }
}

impl FilterCondition {
    pub fn matches(&self, ctx: &FilterContext) -> bool {
        let value = match ctx.resolve_field(&self.field, self.json_path.as_deref()) {
            Some(v) => v,
            None => return false,
        };
        match &self.op {
            MatchOp::Equals(expected) => &value == expected,
            MatchOp::Matches(re) => re.is_match(&value),
        }
    }
}

pub struct FilterContext {
    pub http_method: Option<String>,
    pub http_path: Option<String>,
    pub http_headers: Option<serde_json::Map<String, Value>>,
    pub http_query: Option<serde_json::Map<String, Value>>,
    pub http_body: Option<Value>,
    pub queue_body: Option<Value>,
    pub queue_properties: Option<serde_json::Map<String, Value>>,
}

impl FilterContext {
    pub fn resolve_field(&self, field: &str, json_path: Option<&str>) -> Option<String> {
        let namespace = fields::namespace_of(field);
        match namespace {
            "http" => self.resolve_http_field(field, json_path),
            "queue" => self.resolve_queue_field(field, json_path),
            _ => None,
        }
    }

    fn resolve_http_field(&self, field: &str, json_path: Option<&str>) -> Option<String> {
        match field {
            "http.method" => self.http_method.clone(),
            "http.path" => self.http_path.clone(),
            "http.body" => self.extract_with_json_path(self.http_body.as_ref()?, json_path),
            _ => {
                if let Some((_, key)) = fields::split_header_or_property(field) {
                    if field.starts_with("http.header.") {
                        self.http_headers
                            .as_ref()?
                            .get(key)
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                    } else if field.starts_with("http.query.") {
                        self.http_query
                            .as_ref()?
                            .get(key)
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
        }
    }

    fn resolve_queue_field(&self, field: &str, json_path: Option<&str>) -> Option<String> {
        match field {
            "queue.body" => self.extract_with_json_path(self.queue_body.as_ref()?, json_path),
            _ => {
                if let Some((_, key)) = fields::split_header_or_property(field) {
                    if field.starts_with("queue.property.") {
                        self.queue_properties
                            .as_ref()?
                            .get(key)
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
        }
    }

    fn extract_with_json_path(&self, value: &Value, json_path: Option<&str>) -> Option<String> {
        if let Some(path) = json_path {
            let stripped = path.strip_prefix('$').unwrap_or(path);
            let stripped = stripped.strip_prefix('.').unwrap_or(stripped);
            let keys: Vec<&str> = stripped.split('.').collect();
            let mut current = value;
            for key in &keys {
                current = current.as_object()?.get(*key)?;
            }
            match current {
                Value::String(s) => Some(s.clone()),
                Value::Number(n) => Some(n.to_string()),
                Value::Bool(b) => Some(b.to_string()),
                _ => None,
            }
        } else {
            match value {
                Value::String(s) => Some(s.clone()),
                Value::Number(n) => Some(n.to_string()),
                Value::Bool(b) => Some(b.to_string()),
                _ => None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn make_http_ctx() -> FilterContext {
        let mut headers = serde_json::Map::new();
        headers.insert("X-Request-Id".to_string(), json!("req-123"));
        let mut query = serde_json::Map::new();
        query.insert("page".to_string(), json!("2"));
        FilterContext {
            http_method: Some("POST".to_string()),
            http_path: Some("/orders/123".to_string()),
            http_headers: Some(headers),
            http_query: Some(query),
            http_body: Some(json!({"type": "order", "orderId": "ORD-1"})),
            queue_body: None,
            queue_properties: None,
        }
    }

    #[test]
    fn equals_match() {
        let ctx = make_http_ctx();
        let cond = FilterCondition {
            field: "http.method".to_string(),
            json_path: None,
            op: MatchOp::Equals("POST".to_string()),
        };
        assert!(cond.matches(&ctx));
    }

    #[test]
    fn equals_no_match() {
        let ctx = make_http_ctx();
        let cond = FilterCondition {
            field: "http.method".to_string(),
            json_path: None,
            op: MatchOp::Equals("GET".to_string()),
        };
        assert!(!cond.matches(&ctx));
    }

    #[test]
    fn regex_match() {
        let ctx = make_http_ctx();
        let cond = FilterCondition {
            field: "http.path".to_string(),
            json_path: None,
            op: MatchOp::Matches(Regex::new("^/orders").unwrap()),
        };
        assert!(cond.matches(&ctx));
    }

    #[test]
    fn header_match() {
        let ctx = make_http_ctx();
        let cond = FilterCondition {
            field: "http.header.X-Request-Id".to_string(),
            json_path: None,
            op: MatchOp::Equals("req-123".to_string()),
        };
        assert!(cond.matches(&ctx));
    }

    #[test]
    fn body_json_path_match() {
        let ctx = make_http_ctx();
        let cond = FilterCondition {
            field: "http.body".to_string(),
            json_path: Some("$.type".to_string()),
            op: MatchOp::Matches(Regex::new("^order$").unwrap()),
        };
        assert!(cond.matches(&ctx));
    }

    #[test]
    fn filter_set_all_must_match() {
        let ctx = make_http_ctx();
        let filter = FilterSet {
            conditions: vec![
                FilterCondition {
                    field: "http.method".to_string(),
                    json_path: None,
                    op: MatchOp::Equals("POST".to_string()),
                },
                FilterCondition {
                    field: "http.path".to_string(),
                    json_path: None,
                    op: MatchOp::Matches(Regex::new("^/orders").unwrap()),
                },
            ],
        };
        assert!(filter.matches(&ctx));
    }

    #[test]
    fn filter_set_fails_if_one_fails() {
        let ctx = make_http_ctx();
        let filter = FilterSet {
            conditions: vec![
                FilterCondition {
                    field: "http.method".to_string(),
                    json_path: None,
                    op: MatchOp::Equals("GET".to_string()),
                },
                FilterCondition {
                    field: "http.path".to_string(),
                    json_path: None,
                    op: MatchOp::Matches(Regex::new("^/orders").unwrap()),
                },
            ],
        };
        assert!(!filter.matches(&ctx));
    }
}
