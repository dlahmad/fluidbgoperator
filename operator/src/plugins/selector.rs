use crate::plugins::filter::FilterContext;

#[derive(Clone, Debug)]
pub struct Selector {
    pub field: Option<String>,
    pub json_path: Option<String>,
    pub path_segment: Option<i32>,
    pub value: Option<String>,
}

impl Selector {
    pub fn extract(&self, ctx: &FilterContext) -> Option<String> {
        if let Some(static_val) = &self.value {
            return Some(static_val.clone());
        }
        let field = self.field.as_ref()?;

        if let Some(seg_idx) = self.path_segment
            && field == "http.path"
        {
            return ctx.http_path.as_ref().and_then(|path| {
                path.split('/')
                    .filter(|s| !s.is_empty())
                    .nth((seg_idx - 1) as usize)
                    .map(|s| s.to_string())
            });
        }

        ctx.resolve_field(field, self.json_path.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::plugins::filter::FilterContext;

    fn make_http_ctx() -> FilterContext {
        FilterContext {
            http_method: Some("POST".to_string()),
            http_path: Some("/orders/123/process".to_string()),
            http_headers: None,
            http_query: None,
            http_body: Some(json!({"orderId": "ORD-42"})),
            queue_body: None,
            queue_properties: None,
        }
    }

    #[test]
    fn static_value() {
        let sel = Selector {
            field: None,
            json_path: None,
            path_segment: None,
            value: Some("fixed-id".to_string()),
        };
        assert_eq!(sel.extract(&make_http_ctx()), Some("fixed-id".to_string()));
    }

    #[test]
    fn json_path_extraction() {
        let sel = Selector {
            field: Some("http.body".to_string()),
            json_path: Some("$.orderId".to_string()),
            path_segment: None,
            value: None,
        };
        assert_eq!(sel.extract(&make_http_ctx()), Some("ORD-42".to_string()));
    }

    #[test]
    fn path_segment_extraction() {
        let sel = Selector {
            field: Some("http.path".to_string()),
            json_path: None,
            path_segment: Some(2),
            value: None,
        };
        assert_eq!(sel.extract(&make_http_ctx()), Some("123".to_string()));
    }

    #[test]
    fn header_extraction() {
        let mut headers = serde_json::Map::new();
        headers.insert("X-Test-Id".to_string(), json!("test-abc"));
        let ctx = FilterContext {
            http_method: Some("POST".to_string()),
            http_path: Some("/orders".to_string()),
            http_headers: Some(headers),
            http_query: None,
            http_body: None,
            queue_body: None,
            queue_properties: None,
        };
        let sel = Selector {
            field: Some("http.header.X-Test-Id".to_string()),
            json_path: None,
            path_segment: None,
            value: None,
        };
        assert_eq!(sel.extract(&ctx), Some("test-abc".to_string()));
    }
}
