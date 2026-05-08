use crate::crd::inception_plugin::{PluginRole, PluginRoleConstraints};

pub fn validate_roles(available: &[PluginRole], selected: &[PluginRole]) -> Result<(), String> {
    if selected.is_empty() {
        return Err("inception point must select at least one role".to_string());
    }

    for (idx, role) in selected.iter().enumerate() {
        if selected.iter().skip(idx + 1).any(|other| other == role) {
            return Err(format!(
                "role '{}' is selected more than once",
                role_name(role)
            ));
        }
    }

    for role in selected {
        if !available.contains(role) {
            return Err(format!(
                "role '{}' is not supported by the selected plugin; supported roles: {}",
                role_name(role),
                role_list(available)
            ));
        }
    }
    Ok(())
}

pub fn validate_role_constraints(
    selected: &[PluginRole],
    constraints: Option<&PluginRoleConstraints>,
) -> Result<(), String> {
    let Some(constraints) = constraints else {
        return Ok(());
    };

    for group in &constraints.mutually_exclusive {
        let active = group
            .roles
            .iter()
            .filter(|role| selected.contains(role))
            .collect::<Vec<_>>();
        if active.len() > 1 {
            let reason = group
                .reason
                .as_deref()
                .map(|reason| format!(" {reason}"))
                .unwrap_or_default();
            return Err(format!(
                "unsupported role combination: roles {} are mutually exclusive for the selected plugin.{}",
                active
                    .iter()
                    .map(|role| role_name(role))
                    .collect::<Vec<_>>()
                    .join(", "),
                reason
            ));
        }
    }

    Ok(())
}

pub fn validate_role_selection(
    available: &[PluginRole],
    selected: &[PluginRole],
    constraints: Option<&PluginRoleConstraints>,
) -> Result<(), String> {
    validate_roles(available, selected)?;
    validate_role_constraints(selected, constraints)
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

fn role_name(role: &PluginRole) -> &'static str {
    match role {
        PluginRole::Duplicator => "duplicator",
        PluginRole::Splitter => "splitter",
        PluginRole::Combiner => "combiner",
        PluginRole::Observer => "observer",
        PluginRole::Mock => "mock",
        PluginRole::Writer => "writer",
        PluginRole::Consumer => "consumer",
    }
}

fn role_list(roles: &[PluginRole]) -> String {
    roles.iter().map(role_name).collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crd::inception_plugin::MutuallyExclusiveRoleGroup;

    #[test]
    fn empty_roles_rejected() {
        assert!(validate_roles(&[PluginRole::Observer], &[]).is_err());
    }

    #[test]
    fn unsupported_role_rejected() {
        assert!(validate_roles(&[PluginRole::Observer], &[PluginRole::Duplicator]).is_err());
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
    fn duplicate_role_rejected() {
        let err = validate_roles(
            &[PluginRole::Observer],
            &[PluginRole::Observer, PluginRole::Observer],
        )
        .expect_err("duplicate role must be rejected");
        assert!(err.contains("selected more than once"));
    }

    #[test]
    fn mutually_exclusive_role_combination_rejected() {
        let constraints = PluginRoleConstraints {
            mutually_exclusive: vec![MutuallyExclusiveRoleGroup {
                roles: vec![
                    PluginRole::Duplicator,
                    PluginRole::Splitter,
                    PluginRole::Combiner,
                    PluginRole::Consumer,
                ],
                reason: Some("Only one movement loop can own a transport resource.".to_string()),
            }],
        };

        let err = validate_role_selection(
            &[
                PluginRole::Duplicator,
                PluginRole::Splitter,
                PluginRole::Combiner,
                PluginRole::Observer,
                PluginRole::Writer,
                PluginRole::Consumer,
            ],
            &[
                PluginRole::Combiner,
                PluginRole::Observer,
                PluginRole::Writer,
                PluginRole::Splitter,
            ],
            Some(&constraints),
        )
        .expect_err("conflicting movement roles must be rejected");

        assert!(err.contains("splitter, combiner"));
        assert!(err.contains("mutually exclusive"));
    }

    #[test]
    fn additive_roles_are_accepted_with_one_constrained_role() {
        let constraints = PluginRoleConstraints {
            mutually_exclusive: vec![MutuallyExclusiveRoleGroup {
                roles: vec![
                    PluginRole::Duplicator,
                    PluginRole::Splitter,
                    PluginRole::Combiner,
                    PluginRole::Consumer,
                ],
                reason: None,
            }],
        };

        assert!(
            validate_role_selection(
                &[
                    PluginRole::Duplicator,
                    PluginRole::Splitter,
                    PluginRole::Combiner,
                    PluginRole::Observer,
                    PluginRole::Writer,
                    PluginRole::Consumer,
                ],
                &[
                    PluginRole::Combiner,
                    PluginRole::Observer,
                    PluginRole::Writer
                ],
                Some(&constraints),
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
        assert!(validate_field_namespace("event.body", &["http".to_string()]).is_err());
    }
}
