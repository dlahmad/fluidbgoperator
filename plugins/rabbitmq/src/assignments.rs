use fluidbg_plugin_sdk::{AssignmentKind, AssignmentTarget, PluginRole, PropertyAssignment};

use crate::config::{Config, has_role};

pub(crate) fn build_prepare_assignments(
    config: &Config,
    roles: &[PluginRole],
) -> Vec<PropertyAssignment> {
    let mut assignments = Vec::new();
    if has_role(roles, PluginRole::Duplicator)
        && let Some(duplicator) = config.duplicator.as_ref()
    {
        push_queue_assignment(
            &mut assignments,
            AssignmentTarget::Green,
            &duplicator.green_input_queue_env_var,
            &duplicator.green_input_queue,
        );
        push_queue_assignment(
            &mut assignments,
            AssignmentTarget::Blue,
            &duplicator.blue_input_queue_env_var,
            &duplicator.blue_input_queue,
        );
    }
    if has_role(roles, PluginRole::Splitter)
        && let Some(splitter) = config.splitter.as_ref()
    {
        push_queue_assignment(
            &mut assignments,
            AssignmentTarget::Green,
            &splitter.green_input_queue_env_var,
            &splitter.green_input_queue,
        );
        push_queue_assignment(
            &mut assignments,
            AssignmentTarget::Blue,
            &splitter.blue_input_queue_env_var,
            &splitter.blue_input_queue,
        );
    }
    if has_role(roles, PluginRole::Combiner)
        && let Some(combiner) = config.combiner.as_ref()
    {
        push_queue_assignment(
            &mut assignments,
            AssignmentTarget::Green,
            &combiner.green_output_queue_env_var,
            &combiner.green_output_queue,
        );
        push_queue_assignment(
            &mut assignments,
            AssignmentTarget::Blue,
            &combiner.blue_output_queue_env_var,
            &combiner.blue_output_queue,
        );
    }
    push_temp_queue_declaration_assignments(&mut assignments, config);
    assignments
}

pub(crate) fn build_cleanup_assignments(
    config: &Config,
    roles: &[PluginRole],
) -> Vec<PropertyAssignment> {
    let mut assignments = Vec::new();
    if has_role(roles, PluginRole::Duplicator)
        && let Some(duplicator) = config.duplicator.as_ref()
    {
        push_queue_assignment(
            &mut assignments,
            AssignmentTarget::Green,
            &duplicator.green_input_queue_env_var,
            &duplicator.input_queue,
        );
        push_queue_assignment(
            &mut assignments,
            AssignmentTarget::Blue,
            &duplicator.blue_input_queue_env_var,
            &duplicator.input_queue,
        );
    }
    if has_role(roles, PluginRole::Splitter)
        && let Some(splitter) = config.splitter.as_ref()
    {
        push_queue_assignment(
            &mut assignments,
            AssignmentTarget::Green,
            &splitter.green_input_queue_env_var,
            &splitter.input_queue,
        );
        push_queue_assignment(
            &mut assignments,
            AssignmentTarget::Blue,
            &splitter.blue_input_queue_env_var,
            &splitter.input_queue,
        );
    }
    if has_role(roles, PluginRole::Combiner)
        && let Some(combiner) = config.combiner.as_ref()
    {
        push_queue_assignment(
            &mut assignments,
            AssignmentTarget::Green,
            &combiner.green_output_queue_env_var,
            &combiner.output_queue,
        );
        push_queue_assignment(
            &mut assignments,
            AssignmentTarget::Blue,
            &combiner.blue_output_queue_env_var,
            &combiner.output_queue,
        );
    }
    assignments
}

pub(crate) fn build_drain_assignments(
    config: &Config,
    roles: &[PluginRole],
) -> Vec<PropertyAssignment> {
    build_cleanup_assignments(config, roles)
}

fn push_queue_assignment(
    assignments: &mut Vec<PropertyAssignment>,
    target: AssignmentTarget,
    env_name: &Option<String>,
    queue: &Option<String>,
) {
    if let (Some(env_name), Some(queue)) = (env_name, queue) {
        assignments.push(PropertyAssignment {
            target,
            kind: AssignmentKind::Env,
            name: env_name.clone(),
            value: queue.clone(),
            container_name: None,
        });
    }
}

fn push_temp_queue_declaration_assignments(
    assignments: &mut Vec<PropertyAssignment>,
    config: &Config,
) {
    let durable = config
        .queue_declaration
        .durable
        .unwrap_or(false)
        .to_string();
    let arguments = serde_json::to_string(&config.queue_declaration.arguments)
        .unwrap_or_else(|_| "{}".to_string());

    for target in [AssignmentTarget::Green, AssignmentTarget::Blue] {
        push_env_assignment(assignments, target, "AMQP_TEMP_QUEUE_DURABLE", &durable);
        push_env_assignment(
            assignments,
            target,
            "AMQP_TEMP_QUEUE_ARGUMENTS_JSON",
            &arguments,
        );
    }
}

fn push_env_assignment(
    assignments: &mut Vec<PropertyAssignment>,
    target: AssignmentTarget,
    name: &str,
    value: &str,
) {
    assignments.push(PropertyAssignment {
        target,
        kind: AssignmentKind::Env,
        name: name.to_string(),
        value: value.to_string(),
        container_name: None,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn prepare_assignments_include_temp_queue_declaration_metadata() {
        let config: Config = serde_json::from_value(json!({
            "queueDeclaration": {
                "durable": true,
                "arguments": {
                    "x-message-ttl": 600000
                }
            },
            "shadowQueue": {
                "suffix": "_dlq"
            }
        }))
        .unwrap();

        let assignments = build_prepare_assignments(&config, &[PluginRole::Duplicator]);

        assert!(assignments.iter().any(|assignment| {
            assignment.target == AssignmentTarget::Green
                && assignment.name == "AMQP_TEMP_QUEUE_DURABLE"
                && assignment.value == "true"
        }));
        assert!(assignments.iter().any(|assignment| {
            assignment.target == AssignmentTarget::Blue
                && assignment.name == "AMQP_TEMP_QUEUE_ARGUMENTS_JSON"
                && assignment.value.contains("x-message-ttl")
        }));
    }
}
