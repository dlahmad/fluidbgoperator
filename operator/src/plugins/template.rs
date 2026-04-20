pub fn render_config_template(
    template: &str,
    config: &serde_json::Value,
) -> Result<String, String> {
    let mut result = template.to_string();
    if let serde_json::Value::Object(map) = config {
        for (key, val) in map {
            let placeholder = format!("{{{{{}}}}}", key);
            let replacement = match val {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::Bool(b) => b.to_string(),
                _ => continue,
            };
            result = result.replace(&placeholder, &replacement);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn simple_template() {
        let template = "http://localhost:{{proxyPort}}";
        let config = json!({"proxyPort": 8080, "realEndpoint": "http://upstream"});
        let result = render_config_template(template, &config).unwrap();
        assert_eq!(result, "http://localhost:8080");
    }

    #[test]
    fn multiple_placeholders() {
        let template = "{{host}}:{{port}}";
        let config = json!({"host": "localhost", "port": 9090});
        let result = render_config_template(template, &config).unwrap();
        assert_eq!(result, "localhost:9090");
    }

    #[test]
    fn no_placeholders() {
        let template = "static value";
        let config = json!({});
        let result = render_config_template(template, &config).unwrap();
        assert_eq!(result, "static value");
    }
}
