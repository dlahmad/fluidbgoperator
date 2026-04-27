mod draining;
mod inception;
mod rollout;
mod traffic;

pub(super) use draining::reconcile_draining;
pub(super) use inception::ensure_inception_resources;
#[cfg(test)]
pub(super) use rollout::select_previous_green_for_promotion;
pub(super) use rollout::{
    begin_draining_after_promotion, begin_draining_after_rollback,
    bootstrap_initial_green_if_empty, ensure_declared_deployments, promote,
};
#[cfg(test)]
pub(super) use traffic::validate_progressive_splitter_plugin;
pub(super) use traffic::{
    apply_splitter_traffic_percent, initialize_splitter_traffic,
    validate_progressive_shifting_support,
};
