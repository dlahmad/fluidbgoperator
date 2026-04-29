mod bootstrap;
mod force_delete;
mod force_replace;
mod helm_cleanup;
mod http_proxy;
mod namespace_deletion;
mod progressive_splitter;
mod progressive_unsupported;
mod rabbitmq_promotion;
mod rollback_shadow_recovery;
mod support;

use anyhow::Result;

use crate::harness::E2eHarness;

const E2E_BGDS: &[&str] = &[
    "order-processor-bootstrap",
    "order-processor-upgrade",
    "order-processor-failing-upgrade",
    "order-processor-progressive-unsupported",
    "order-processor-progressive-upgrade",
    "order-processor-http-upgrade",
    "order-processor-force-replace",
    "order-processor-force-delete",
];

pub async fn run_full_suite(harness: &mut E2eHarness) -> Result<()> {
    let bootstrap = bootstrap::bootstrap_initial_green(harness).await?;
    let promoted = rabbitmq_promotion::successful_rabbitmq_promotion(harness, &bootstrap).await?;
    rollback_shadow_recovery::rollback_recovers_temporary_and_shadow_queues(harness, &promoted)
        .await?;
    progressive_unsupported::progressive_support_is_enforced(harness).await?;
    let progressive =
        progressive_splitter::progressive_splitter_promotion(harness, &promoted).await?;
    let http = http_proxy::http_proxy_observer_promotion(harness, &progressive).await?;
    let force_replaced =
        force_replace::force_replace_drains_before_new_generation(harness, &http).await?;
    force_delete::force_deleted_bgd_is_cleaned_as_orphan(harness, &force_replaced).await?;
    namespace_deletion::namespace_deletion_does_not_deadlock_bgd_finalizers(harness).await?;
    helm_cleanup::helm_uninstall_cleans_operator_resources(harness, E2E_BGDS).await
}
