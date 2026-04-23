use crate::crd::inception_plugin::PluginRole;

pub fn validate_roles(available: &[PluginRole], selected: &[PluginRole]) -> Result<(), String> {
    if selected.is_empty() {
        return Err("inception point must select at least one role".to_string());
    }

    for role in selected {
        if !available.contains(role) {
            return Err(format!(
                "role '{role:?}' is not supported by the selected plugin; supported roles: {:?}",
                available
            ));
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
    fn empty_roles_rejected() {
        assert!(validate_roles(&[PluginRole::Observer], &[]).is_err());
    }

    #[test]
    fn unsupported_role_rejected() {
        assert!(validate_roles(&[PluginRole::Observer], &[PluginRole::Splitter]).is_err());
    }

    #[test]
    fn supported_roles_accepted() {
        assert!(
            validate_roles(
                &[PluginRole::Splitter, PluginRole::Observer],
                &[PluginRole::Splitter, PluginRole::Observer]
            )
            .is_ok()
        );
    }

    #[test]
    fn field_namespace_valid() {
        assert!(validate_field_namespace("http.method", &["http".to_string()]).is_ok());
    }

    #[test]
    fn field_namespace_invalid() {
        assert!(validate_field_namespace("queue.body", &["http".to_string()]).is_err());
    }
}
