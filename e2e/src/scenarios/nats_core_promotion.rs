use std::time::Duration;

use anyhow::{Result, bail};

use crate::harness::E2eHarness;
use crate::kube::PodHttpRequest;
use crate::status::bgd_status;

use super::support::{assert_condition, wait_for_terminal_phase, wait_for_tracked_cases};

pub async fn successful_core_nats_promotion(harness: &mut E2eHarness) -> Result<String> {
    let cfg = harness.config.clone();
    harness
        .kube
        .wait_no_inception_resources(&cfg.namespace)
        .await?;
    harness
        .kube
        .delete_labeled_resources(&cfg.namespace, "fluidbg.io/name=nats-core-processor")
        .await?;
    harness
        .kube
        .wait_label_resource_count(
            &cfg.namespace,
            "fluidbg.io/name=nats-core-processor",
            0,
            Duration::from_secs(60),
        )
        .await?;
    harness
        .kube
        .apply_file(&cfg.deploy_file("nats-core-promotion/initial.yaml"))
        .await?;
    let initial = harness
        .kube
        .wait_bgd_generated_name("order-processor-nats-core", &cfg.namespace)
        .await?;
    harness
        .kube
        .wait_bgd_phase("order-processor-nats-core", &cfg.namespace, "Completed", 30)
        .await?;
    harness
        .kube
        .rollout_status(&initial, &cfg.namespace, Duration::from_secs(120))
        .await?;

    harness
        .kube
        .apply_file(&cfg.deploy_file("nats-core-promotion/upgrade.yaml"))
        .await?;
    tokio::time::sleep(Duration::from_secs(5)).await;
    let upgraded = harness
        .kube
        .wait_bgd_generated_name("order-processor-nats-core", &cfg.namespace)
        .await?;
    let input_plugin = harness
        .kube
        .wait_inception_deployment_name(
            "order-processor-nats-core",
            "incoming-core-nats-orders",
            &cfg.namespace,
        )
        .await?;
    let output_plugin = harness
        .kube
        .wait_inception_deployment_name(
            "order-processor-nats-core",
            "outgoing-core-nats-results",
            &cfg.namespace,
        )
        .await?;
    let test_deployment = harness
        .kube
        .wait_test_deployment_name(
            "order-processor-nats-core",
            "test-container",
            &cfg.namespace,
        )
        .await?;

    for rollout in [
        &initial,
        &upgraded,
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
        .wait_bgd_phase("order-processor-nats-core", &cfg.namespace, "Observing", 60)
        .await?;

    let selector = harness
        .kube
        .deployment_pod_selector(&test_deployment, &cfg.namespace)
        .await?;
    let mut reached_required_evidence = false;
    for i in 1..=12 {
        let response = harness
            .kube
            .pod_http_by_selector(
                &cfg.namespace,
                &selector,
                8080,
                PodHttpRequest {
                    method: "POST",
                    path: "/trigger",
                    basic_auth: None,
                    headers: Vec::new(),
                    body: Some(serde_json::json!({
                        "testId": format!("nats-core-order-{i}")
                    })),
                },
            )
            .await?;
        if !(200..300).contains(&response.status) {
            bail!(
                "core NATS verifier trigger returned HTTP {}",
                response.status
            );
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
        let status = bgd_status(
            &harness
                .kube
                .bgd_json("order-processor-nats-core", &cfg.namespace)
                .await?,
        );
        if status.tracked_cases() >= 3 && status.test_cases_passed >= 3 {
            reached_required_evidence = true;
            break;
        }
        if status.phase == "RolledBack" {
            bail!("order-processor-nats-core rolled back while producing core NATS evidence");
        }
    }

    if !reached_required_evidence {
        wait_for_tracked_cases(harness, "order-processor-nats-core", 3, 3, 90).await?;
    }
    wait_for_terminal_phase(harness, "order-processor-nats-core", "Completed", 45).await?;
    harness
        .kube
        .wait_deleted(
            "deployment",
            &initial,
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
        .wait_no_inception_resources(&cfg.namespace)
        .await?;
    harness
        .kube
        .wait_deployment_label(&upgraded, &cfg.namespace, "fluidbg.io/green", "true")
        .await?;

    let status = harness
        .kube
        .bgd_json("order-processor-nats-core", &cfg.namespace)
        .await?;
    assert_condition(&status, "order-processor-nats-core", "Ready", "True")?;
    assert_condition(&status, "order-processor-nats-core", "Progressing", "False")?;
    assert_condition(&status, "order-processor-nats-core", "Degraded", "False")?;
    Ok(upgraded)
}
