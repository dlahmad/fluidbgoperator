use crate::models::PluginRole;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueueWorkerRole {
    Input,
    Combiner,
}

pub fn queue_worker_role(roles: &[PluginRole]) -> Result<Option<QueueWorkerRole>, String> {
    let movement_roles = [
        PluginRole::Duplicator,
        PluginRole::Splitter,
        PluginRole::Combiner,
        PluginRole::Consumer,
    ];
    let active = movement_roles
        .into_iter()
        .filter(|role| roles.contains(role))
        .collect::<Vec<_>>();

    if active.len() > 1 {
        return Err(format!(
            "queue movement roles are mutually exclusive; selected roles: {active:?}"
        ));
    }

    Ok(match active.first().copied() {
        Some(PluginRole::Combiner) => Some(QueueWorkerRole::Combiner),
        Some(_) => Some(QueueWorkerRole::Input),
        None => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observer_and_writer_do_not_select_a_queue_worker() {
        assert_eq!(
            queue_worker_role(&[PluginRole::Observer, PluginRole::Writer]).unwrap(),
            None
        );
    }

    #[test]
    fn observer_and_writer_are_additive_to_input_roles() {
        assert_eq!(
            queue_worker_role(&[
                PluginRole::Splitter,
                PluginRole::Observer,
                PluginRole::Writer
            ])
            .unwrap(),
            Some(QueueWorkerRole::Input)
        );
    }

    #[test]
    fn observer_and_writer_are_additive_to_combiner() {
        assert_eq!(
            queue_worker_role(&[
                PluginRole::Combiner,
                PluginRole::Observer,
                PluginRole::Writer
            ])
            .unwrap(),
            Some(QueueWorkerRole::Combiner)
        );
    }

    #[test]
    fn queue_movement_roles_are_exclusive() {
        assert!(queue_worker_role(&[PluginRole::Splitter, PluginRole::Combiner]).is_err());
        assert!(queue_worker_role(&[PluginRole::Duplicator, PluginRole::Consumer]).is_err());
    }
}
