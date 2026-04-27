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
