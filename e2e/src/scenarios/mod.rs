mod bootstrap;
mod force_delete;
mod force_replace;
mod helm_cleanup;
mod http_proxy;
mod namespace_deletion;
mod nats_core_promotion;
mod nats_promotion;
mod progressive_splitter;
mod progressive_unsupported;
mod rabbitmq_promotion;
mod rollback_shadow_recovery;
mod support;

use std::future::Future;
use std::time::Duration;

use anyhow::{Context, Result};

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
    "order-processor-nats",
    "order-processor-nats-core",
];

pub async fn run_full_suite(harness: &mut E2eHarness) -> Result<()> {
    let bootstrap = run_scenario(
        "bootstrap initial green",
        bootstrap::bootstrap_initial_green(harness),
    )
    .await?;
    let promoted = run_scenario(
        "successful RabbitMQ promotion",
        rabbitmq_promotion::successful_rabbitmq_promotion(harness, &bootstrap),
    )
    .await?;
    run_scenario(
        "rollback shadow recovery",
        rollback_shadow_recovery::rollback_recovers_temporary_and_shadow_queues(harness, &promoted),
    )
    .await?;
    run_scenario(
        "progressive unsupported validation",
        progressive_unsupported::progressive_support_is_enforced(harness),
    )
    .await?;
    let progressive = run_scenario(
        "progressive splitter promotion",
        progressive_splitter::progressive_splitter_promotion(harness, &promoted),
    )
    .await?;
    let http = run_scenario(
        "HTTP proxy observer promotion",
        http_proxy::http_proxy_observer_promotion(harness, &progressive),
    )
    .await?;
    let _nats = run_scenario(
        "NATS JetStream promotion",
        nats_promotion::successful_nats_promotion(harness),
    )
    .await?;
    let _core_nats = run_scenario(
        "core NATS best-effort promotion",
        nats_core_promotion::successful_core_nats_promotion(harness),
    )
    .await?;
    let force_replaced = run_scenario(
        "forced replacement with drain",
        force_replace::force_replace_drains_before_new_generation(harness, &http),
    )
    .await?;
    run_scenario(
        "force-deleted BGD orphan cleanup",
        force_delete::force_deleted_bgd_is_cleaned_as_orphan(harness, &force_replaced),
    )
    .await?;
    run_scenario(
        "namespace deletion finalizer safety",
        namespace_deletion::namespace_deletion_does_not_deadlock_bgd_finalizers(harness),
    )
    .await?;
    run_scenario(
        "Helm cleanup",
        helm_cleanup::helm_uninstall_cleans_operator_resources(harness, E2E_BGDS),
    )
    .await
}

async fn run_scenario<T, F>(name: &str, future: F) -> Result<T>
where
    F: Future<Output = Result<T>>,
{
    const SCENARIO_TIMEOUT: Duration = Duration::from_secs(8 * 60);

    eprintln!("e2e scenario: {name}");
    tokio::time::timeout(SCENARIO_TIMEOUT, future)
        .await
        .with_context(|| format!("e2e scenario '{name}' timed out after {SCENARIO_TIMEOUT:?}"))?
}
