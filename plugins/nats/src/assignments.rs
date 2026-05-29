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
        push_subject_assignment(
            &mut assignments,
            AssignmentTarget::Green,
            &duplicator.green_input_subject_env_var,
            &duplicator.green_input_subject,
        );
        push_subject_assignment(
            &mut assignments,
            AssignmentTarget::Blue,
            &duplicator.blue_input_subject_env_var,
            &duplicator.blue_input_subject,
        );
        push_subject_assignment(
            &mut assignments,
            AssignmentTarget::Green,
            &duplicator.green_queue_group_env_var,
            &duplicator.green_queue_group,
        );
        push_subject_assignment(
            &mut assignments,
            AssignmentTarget::Blue,
            &duplicator.blue_queue_group_env_var,
            &duplicator.blue_queue_group,
        );
    }
    if has_role(roles, PluginRole::Splitter)
        && let Some(splitter) = config.splitter.as_ref()
    {
        push_subject_assignment(
            &mut assignments,
            AssignmentTarget::Green,
            &splitter.green_input_subject_env_var,
            &splitter.green_input_subject,
        );
        push_subject_assignment(
            &mut assignments,
            AssignmentTarget::Blue,
            &splitter.blue_input_subject_env_var,
            &splitter.blue_input_subject,
        );
        push_subject_assignment(
            &mut assignments,
            AssignmentTarget::Green,
            &splitter.green_queue_group_env_var,
            &splitter.green_queue_group,
        );
        push_subject_assignment(
            &mut assignments,
            AssignmentTarget::Blue,
            &splitter.blue_queue_group_env_var,
            &splitter.blue_queue_group,
        );
    }
    if has_role(roles, PluginRole::Combiner)
        && let Some(combiner) = config.combiner.as_ref()
    {
        push_subject_assignment(
            &mut assignments,
            AssignmentTarget::Green,
            &combiner.green_output_subject_env_var,
            &combiner.green_output_subject,
        );
        push_subject_assignment(
            &mut assignments,
            AssignmentTarget::Blue,
            &combiner.blue_output_subject_env_var,
            &combiner.blue_output_subject,
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
        push_subject_assignment(
            &mut assignments,
            AssignmentTarget::Green,
            &duplicator.green_input_subject_env_var,
            &duplicator.input_subject,
        );
        push_subject_assignment(
            &mut assignments,
            AssignmentTarget::Blue,
            &duplicator.blue_input_subject_env_var,
            &duplicator.input_subject,
        );
        push_subject_assignment(
            &mut assignments,
            AssignmentTarget::Green,
            &duplicator.green_queue_group_env_var,
            &duplicator.queue_group,
        );
        push_subject_assignment(
            &mut assignments,
            AssignmentTarget::Blue,
            &duplicator.blue_queue_group_env_var,
            &duplicator.queue_group,
        );
    }
    if has_role(roles, PluginRole::Splitter)
        && let Some(splitter) = config.splitter.as_ref()
    {
        push_subject_assignment(
            &mut assignments,
            AssignmentTarget::Green,
            &splitter.green_input_subject_env_var,
            &splitter.input_subject,
        );
        push_subject_assignment(
            &mut assignments,
            AssignmentTarget::Blue,
            &splitter.blue_input_subject_env_var,
            &splitter.input_subject,
        );
        push_subject_assignment(
            &mut assignments,
            AssignmentTarget::Green,
            &splitter.green_queue_group_env_var,
            &splitter.queue_group,
        );
        push_subject_assignment(
            &mut assignments,
            AssignmentTarget::Blue,
            &splitter.blue_queue_group_env_var,
            &splitter.queue_group,
        );
    }
    if has_role(roles, PluginRole::Combiner)
        && let Some(combiner) = config.combiner.as_ref()
    {
        push_subject_assignment(
            &mut assignments,
            AssignmentTarget::Green,
            &combiner.green_output_subject_env_var,
            &combiner.output_subject,
        );
        push_subject_assignment(
            &mut assignments,
            AssignmentTarget::Blue,
            &combiner.blue_output_subject_env_var,
            &combiner.output_subject,
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

fn push_subject_assignment(
    assignments: &mut Vec<PropertyAssignment>,
    target: AssignmentTarget,
    env_name: &Option<String>,
    subject: &Option<String>,
) {
    if let (Some(env_name), Some(subject)) = (env_name, subject) {
        assignments.push(PropertyAssignment {
            target,
            kind: AssignmentKind::Env,
            name: env_name.clone(),
            value: subject.clone(),
            container_name: None,
        });
    }
}
