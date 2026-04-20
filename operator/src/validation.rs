use crate::crd::inception_plugin::{Direction, PluginMode};

pub fn validate_mode_direction(mode: &PluginMode, directions: &[Direction]) -> Result<(), String> {
    if matches!(mode, PluginMode::RerouteMock) {
        if directions.len() == 1 && directions[0] == Direction::Ingress {
            return Err(
                "reroute-mock with ingress direction is invalid: it would replace blue itself"
                    .to_string(),
            );
        }
        if directions.contains(&Direction::Ingress) && directions.contains(&Direction::Egress) {
            return Err(
                "reroute-mock with both directions is invalid: ingress would replace blue itself"
                    .to_string(),
            );
        }
    }
    Ok(())
}

pub fn validate_field_namespace(
    field: &str,
    supported_namespaces: &[String],
) -> Result<(), String> {
    let namespace = field.split('.').next().unwrap_or(field);
    if !supported_namespaces.iter().any(|ns| ns == namespace) {
        return Err(format!(
            "field '{}' uses namespace '{}' which is not in supported namespaces: {:?}",
            field, namespace, supported_namespaces
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reroute_mock_ingress_rejected() {
        let mode = PluginMode::RerouteMock;
        let directions = vec![Direction::Ingress];
        assert!(validate_mode_direction(&mode, &directions).is_err());
    }

    #[test]
    fn reroute_mock_egress_accepted() {
        let mode = PluginMode::RerouteMock;
        let directions = vec![Direction::Egress];
        assert!(validate_mode_direction(&mode, &directions).is_ok());
    }

    #[test]
    fn reroute_mock_both_rejected() {
        let mode = PluginMode::RerouteMock;
        let directions = vec![Direction::Ingress, Direction::Egress];
        assert!(validate_mode_direction(&mode, &directions).is_err());
    }

    #[test]
    fn trigger_ingress_accepted() {
        let mode = PluginMode::Trigger;
        let directions = vec![Direction::Ingress];
        assert!(validate_mode_direction(&mode, &directions).is_ok());
    }

    #[test]
    fn field_namespace_valid() {
        let field = "http.method";
        let namespaces = vec!["http".to_string()];
        assert!(validate_field_namespace(field, &namespaces).is_ok());
    }

    #[test]
    fn field_namespace_invalid() {
        let field = "queue.body";
        let namespaces = vec!["http".to_string()];
        assert!(validate_field_namespace(field, &namespaces).is_err());
    }
}
