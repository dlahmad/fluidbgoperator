use jsonschema::validator_for;
use serde_json::Value;

pub fn validate_config_against_schema(
    config: &Value,
    schema: &Value,
) -> std::result::Result<(), Vec<String>> {
    let validator = validator_for(schema).map_err(|e| vec![format!("invalid schema: {}", e)])?;
    if validator.is_valid(config) {
        Ok(())
    } else {
        let msgs: Vec<String> = validator
            .validate(config)
            .err()
            .map(|e| vec![e.to_string()])
            .unwrap_or_default();
        if msgs.is_empty() {
            Err(vec!["config validation failed".to_string()])
        } else {
            Err(msgs)
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn valid_config_passes() {
        let schema = json!({
            "type": "object",
            "required": ["proxyPort"],
            "properties": {
                "proxyPort": { "type": "integer" },
                "realEndpoint": { "type": "string" }
            }
        });
        let config = json!({"proxyPort": 8080, "realEndpoint": "http://upstream"});
        assert!(validate_config_against_schema(&config, &schema).is_ok());
    }

    #[test]
    fn missing_required_field_fails() {
        let schema = json!({
            "type": "object",
            "required": ["proxyPort"],
            "properties": {
                "proxyPort": { "type": "integer" }
            }
        });
        let config = json!({"realEndpoint": "http://upstream"});
        assert!(validate_config_against_schema(&config, &schema).is_err());
    }

    #[test]
    fn wrong_type_fails() {
        let schema = json!({
            "type": "object",
            "required": ["proxyPort"],
            "properties": {
                "proxyPort": { "type": "integer" }
            }
        });
        let config = json!({"proxyPort": "not-a-number"});
        assert!(validate_config_against_schema(&config, &schema).is_err());
    }
}
