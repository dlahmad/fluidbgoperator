use std::time::Duration;

use anyhow::{Result, bail};

use crate::harness::E2eHarness;
use crate::status::bgd_status;

use super::support::{unique_token, wait_http_case_verified};

pub async fn http_proxy_observer_promotion(
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
        .apply_file(&cfg.deploy_file("http-proxy/bgd.yaml"))
        .await?;
    tokio::time::sleep(Duration::from_secs(5)).await;
    let deployment = harness
        .kube
        .wait_bgd_generated_name("order-processor-http-upgrade", &cfg.namespace)
        .await?;
    let input_plugin = harness
        .kube
        .wait_inception_deployment_name(
            "order-processor-http-upgrade",
            "incoming-orders",
            &cfg.namespace,
        )
        .await?;
    let proxy_plugin = harness
        .kube
        .wait_inception_deployment_name(
            "order-processor-http-upgrade",
            "http-upstream",
            &cfg.namespace,
        )
        .await?;
    let output_plugin = harness
        .kube
        .wait_inception_deployment_name(
            "order-processor-http-upgrade",
            "outgoing-results",
            &cfg.namespace,
        )
        .await?;
    let test_deployment = harness
        .kube
        .wait_test_deployment_name(
            "order-processor-http-upgrade",
            "test-container",
            &cfg.namespace,
        )
        .await?;
    for rollout in [
        previous_green,
        &deployment,
        &test_deployment,
        &input_plugin,
        &proxy_plugin,
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
            "order-processor-http-upgrade",
            &cfg.namespace,
            "Observing",
            60,
        )
        .await?;
    harness
        .kube
        .wait_deployment_replicas(&deployment, &cfg.namespace, 1)
        .await?;
    let green_input_queue = harness
        .kube
        .get_inception_config_value(
            "order-processor-http-upgrade",
            "incoming-orders",
            &cfg.namespace,
            "duplicator.greenInputQueue",
        )
        .await?;
    let blue_input_queue = harness
        .kube
        .get_inception_config_value(
            "order-processor-http-upgrade",
            "incoming-orders",
            &cfg.namespace,
            "duplicator.blueInputQueue",
        )
        .await?;
    harness
        .rabbitmq
        .wait_for_consumers(&green_input_queue, 1, Duration::from_secs(60))
        .await?;
    harness
        .rabbitmq
        .wait_for_consumers(&blue_input_queue, 1, Duration::from_secs(60))
        .await?;
    let verified_test_id = format!("http-proxy-{}", unique_token("case"));
    harness
        .rabbitmq
        .publish(
            "orders",
            &format!(
                r#"{{"orderId":"{verified_test_id}","type":"order","action":"http-proxy-check"}}"#
            ),
        )
        .await?;
    wait_http_case_verified(&cfg.namespace, &test_deployment, &verified_test_id, 120).await?;

    let promotion_test_id = format!("http-proxy-{}", unique_token("case"));
    harness
        .rabbitmq
        .publish(
            "orders",
            &format!(
                r#"{{"orderId":"{promotion_test_id}","type":"order","action":"http-proxy-check"}}"#
            ),
        )
        .await?;

    for _ in 1..=120 {
        let status_json = harness
            .kube
            .bgd_json("order-processor-http-upgrade", &cfg.namespace)
            .await?;
        let status = bgd_status(&status_json);
        if status.phase == "Completed" && status.test_cases_passed >= 2 {
            break;
        }
        if status.phase == "RolledBack" {
            bail!("HTTP plugin BGD rolled back");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let status = bgd_status(
        &harness
            .kube
            .bgd_json("order-processor-http-upgrade", &cfg.namespace)
            .await?,
    );
    if status.phase != "Completed" || status.test_cases_passed < 2 {
        bail!(
            "expected HTTP plugin BGD Completed with at least two passed tests, got phase={} passed={}",
            status.phase,
            status.test_cases_passed
        );
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
    harness
        .kube
        .wait_deployment_replicas(&deployment, &cfg.namespace, 2)
        .await?;
    Ok(deployment)
}
