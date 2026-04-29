use std::time::Duration;

use anyhow::{Result, bail};

use crate::harness::E2eHarness;
use crate::status::bgd_status;

pub async fn progressive_splitter_promotion(
    harness: &mut E2eHarness,
    previous_green: &str,
) -> Result<String> {
    let cfg = harness.config.clone();
    harness
        .kube
        .wait_no_inception_resources(&cfg.namespace)
        .await?;
    harness
        .kube
        .apply_file(&cfg.deploy_file("progressive-splitter/bgd.yaml"))
        .await?;
    tokio::time::sleep(Duration::from_secs(5)).await;
    let deployment = harness
        .kube
        .wait_bgd_generated_name("order-processor-progressive-upgrade", &cfg.namespace)
        .await?;
    let input_plugin = harness
        .kube
        .wait_inception_deployment_name(
            "order-processor-progressive-upgrade",
            "incoming-orders",
            &cfg.namespace,
        )
        .await?;
    let output_plugin = harness
        .kube
        .wait_inception_deployment_name(
            "order-processor-progressive-upgrade",
            "outgoing-results",
            &cfg.namespace,
        )
        .await?;
    let test_deployment = harness
        .kube
        .wait_test_deployment_name(
            "order-processor-progressive-upgrade",
            "test-container",
            &cfg.namespace,
        )
        .await?;
    for rollout in [
        previous_green,
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
            "order-processor-progressive-upgrade",
            &cfg.namespace,
            "Observing",
            60,
        )
        .await?;
    tokio::time::sleep(Duration::from_secs(5)).await;
    let input_pod_before = harness
        .kube
        .first_pod_name_by_selector(&cfg.namespace, &format!("app={input_plugin}"))
        .await?;

    let mut published = 0;
    let mut live_shift_verified = false;
    let mut delayed_case_published = false;
    let mut drain_saw_pending = false;
    for _ in 1..=180 {
        let status_json = harness
            .kube
            .bgd_json("order-processor-progressive-upgrade", &cfg.namespace)
            .await?;
        let status = bgd_status(&status_json);
        if status.phase == "Draining" && status.test_cases_pending > 0 {
            drain_saw_pending = true;
        }
        if status.current_traffic_percent == 100
            && !live_shift_verified
            && status.phase == "Observing"
        {
            let current_pod = harness
                .kube
                .first_pod_name_by_selector(&cfg.namespace, &format!("app={input_plugin}"))
                .await?;
            if current_pod != input_pod_before {
                bail!(
                    "progressive traffic shift restarted splitter pod: before={input_pod_before} current={current_pod}"
                );
            }
            live_shift_verified = true;
            published += 1;
            publish_progressive_message_with_delay(harness, published, 20).await?;
            delayed_case_published = true;
            let live_shift_target = published + 6;
            while published < live_shift_target {
                published += 1;
                publish_progressive_message(harness, published).await?;
            }
        }
        if status.phase == "Completed" {
            break;
        }
        if status.phase == "RolledBack" {
            bail!("progressive BGD rolled back");
        }
        if !live_shift_verified && status.test_cases_observed < 1 {
            published += 1;
            publish_progressive_message(harness, published).await?;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    let status_json = harness
        .kube
        .bgd_json("order-processor-progressive-upgrade", &cfg.namespace)
        .await?;
    let status = bgd_status(&status_json);
    if status.phase != "Completed" {
        bail!("expected progressive BGD Completed, got {}", status.phase);
    }
    if !delayed_case_published {
        bail!("expected progressive scenario to publish a delayed verification case");
    }
    if !drain_saw_pending {
        bail!("expected progressive drain to wait while a delayed case was pending");
    }
    if status.test_cases_pending != 0 {
        bail!(
            "expected no pending progressive cases after completion, got {}",
            status.test_cases_pending
        );
    }
    if !live_shift_verified {
        bail!("expected to observe live progressive traffic shift before cleanup");
    }
    harness
        .kube
        .wait_deleted(
            "deployment",
            previous_green,
            &cfg.namespace,
            Duration::from_secs(30),
        )
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
    harness
        .kube
        .wait_no_inception_resources(&cfg.namespace)
        .await?;
    harness
        .kube
        .wait_deployment_label(&deployment, &cfg.namespace, "fluidbg.io/green", "true")
        .await?;
    Ok(deployment)
}

async fn publish_progressive_message(harness: &mut E2eHarness, index: u64) -> Result<()> {
    publish_progressive_message_with_delay(harness, index, 0).await
}

async fn publish_progressive_message_with_delay(
    harness: &mut E2eHarness,
    index: u64,
    verify_delay_seconds: u64,
) -> Result<()> {
    let delay_field = if verify_delay_seconds > 0 {
        format!(r#","verifyDelaySeconds":{verify_delay_seconds}"#)
    } else {
        String::new()
    };
    harness
        .rabbitmq
        .publish(
            "orders",
            &format!(
                r#"{{"orderId":"progressive-{index}","type":"progressive-order","action":"process"{delay_field}}}"#
            ),
        )
        .await
}
