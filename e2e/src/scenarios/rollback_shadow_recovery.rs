use std::time::Duration;

use anyhow::{Result, bail};

use crate::harness::E2eHarness;
use crate::status::ensure_no_drain_timeouts;

use super::support::{assert_condition, unique_token, wait_for_terminal_phase};

pub async fn rollback_recovers_temporary_and_shadow_queues(
    harness: &mut E2eHarness,
    current_green: &str,
) -> Result<()> {
    let cfg = harness.config.clone();
    harness
        .kube
        .apply_file(&cfg.deploy_file("rollback-shadow-recovery/bgd.yaml"))
        .await?;
    tokio::time::sleep(Duration::from_secs(5)).await;

    let deployment = harness
        .kube
        .wait_bgd_generated_name("order-processor-failing-upgrade", &cfg.namespace)
        .await?;
    let input_plugin = harness
        .kube
        .wait_inception_deployment_name(
            "order-processor-failing-upgrade",
            "incoming-orders",
            &cfg.namespace,
        )
        .await?;
    let output_plugin = harness
        .kube
        .wait_inception_deployment_name(
            "order-processor-failing-upgrade",
            "outgoing-results",
            &cfg.namespace,
        )
        .await?;
    let test_deployment = harness
        .kube
        .wait_test_deployment_name(
            "order-processor-failing-upgrade",
            "test-container",
            &cfg.namespace,
        )
        .await?;
    for rollout in [
        current_green,
        &deployment,
        &test_deployment,
        &input_plugin,
        &output_plugin,
    ] {
        harness
            .kube
            .rollout_status(rollout, &cfg.namespace, Duration::from_secs(120))
            .await?;
    }
    harness
        .kube
        .wait_bgd_phase(
            "order-processor-failing-upgrade",
            &cfg.namespace,
            "Observing",
            60,
        )
        .await?;

    let green_input_queue = harness
        .kube
        .get_inception_config_value(
            "order-processor-failing-upgrade",
            "incoming-orders",
            &cfg.namespace,
            "duplicator.greenInputQueue",
        )
        .await?;
    let green_output_queue = harness
        .kube
        .get_inception_config_value(
            "order-processor-failing-upgrade",
            "outgoing-results",
            &cfg.namespace,
            "combiner.greenOutputQueue",
        )
        .await?;
    let green_input_shadow_queue = format!("{green_input_queue}_dlq");
    let green_output_shadow_queue = format!("{green_output_queue}_dlq");
    let input_shadow_token = unique_token("input-shadow-rollback");
    let output_shadow_token = unique_token("output-shadow-rollback");
    harness
        .rabbitmq
        .publish(
            &green_input_shadow_queue,
            &format!(r#"{{"shadowToken":"{input_shadow_token}","type":"order"}}"#),
        )
        .await?;
    harness
        .rabbitmq
        .publish(
            &green_output_shadow_queue,
            &format!(r#"{{"shadowToken":"{output_shadow_token}","type":"result"}}"#),
        )
        .await?;

    let recovery_token = "rollback-recovery-1";
    harness.rabbitmq.publish(
        "orders",
        r#"{"type":"order","action":"process","recoveryToken":"rollback-recovery-1","greenInitialProcessingDelaySeconds":30}"#,
    ).await?;
    tokio::time::sleep(Duration::from_secs(2)).await;
    for i in 1..=5 {
        harness.rabbitmq.publish(
            "orders",
            &format!(
                r#"{{"orderId":"fail-{i}","type":"order","action":"process","shouldPass":false,"failureReason":"synthetic failed promotion case"}}"#
            ),
        ).await?;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    wait_for_terminal_phase(harness, "order-processor-failing-upgrade", "RolledBack", 40).await?;
    let status = harness
        .kube
        .bgd_json("order-processor-failing-upgrade", &cfg.namespace)
        .await?;
    ensure_no_drain_timeouts(&status, "order-processor-failing-upgrade")?;
    harness
        .kube
        .wait_deleted(
            "deployment",
            &deployment,
            &cfg.namespace,
            Duration::from_secs(30),
        )
        .await?;
    harness
        .kube
        .wait_deployment_label(current_green, &cfg.namespace, "fluidbg.io/green", "true")
        .await?;
    harness
        .kube
        .wait_deleted(
            "deployment",
            &test_deployment,
            &cfg.namespace,
            Duration::from_secs(30),
        )
        .await?;
    harness
        .kube
        .wait_deleted(
            "service",
            &test_deployment,
            &cfg.namespace,
            Duration::from_secs(30),
        )
        .await?;
    for queue in [
        "orders-green",
        "orders-blue",
        "results-green",
        "results-blue",
        &green_input_shadow_queue,
        &green_output_shadow_queue,
    ] {
        harness.rabbitmq.assert_queue_drained(queue).await?;
    }
    harness
        .kube
        .wait_no_inception_resources(&cfg.namespace)
        .await?;

    let mut recovered = false;
    for _ in 0..30 {
        if harness
            .rabbitmq
            .queue_contains_processed_message("results", recovery_token, current_green)
            .await
            .unwrap_or(false)
        {
            recovered = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    if !recovered {
        bail!("recovered delayed green message was not processed by restored green deployment");
    }

    let mut shadows_recovered = false;
    for _ in 0..30 {
        let input_recovered = harness
            .rabbitmq
            .queue_contains_json_field("orders_dlq", "shadowToken", &input_shadow_token)
            .await
            .unwrap_or(false);
        let output_recovered = harness
            .rabbitmq
            .queue_contains_json_field("results_dlq", "shadowToken", &output_shadow_token)
            .await
            .unwrap_or(false);
        if input_recovered && output_recovered {
            shadows_recovered = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    if !shadows_recovered {
        bail!("shadow queue messages were not moved back to base shadow queues");
    }

    let status = harness
        .kube
        .bgd_json("order-processor-failing-upgrade", &cfg.namespace)
        .await?;
    assert_condition(&status, "order-processor-failing-upgrade", "Ready", "False")?;
    assert_condition(
        &status,
        "order-processor-failing-upgrade",
        "Progressing",
        "False",
    )?;
    assert_condition(
        &status,
        "order-processor-failing-upgrade",
        "Degraded",
        "True",
    )
}
