use std::time::Duration;

use anyhow::{Result, bail};

use crate::harness::E2eHarness;
use crate::kube::PodHttpRequest;
use crate::status::{bgd_status, testcase_flags};

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
    verify_http_plugin_proxy_and_mock(harness, &proxy_plugin, &test_deployment).await?;

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
    wait_http_case_verified(harness, &test_deployment, &verified_test_id, 120).await?;

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
    eprintln!("HTTP proxy scenario: waiting for previous green deployment cleanup");
    harness
        .kube
        .wait_deleted(
            "deployment",
            previous_green,
            &cfg.namespace,
            Duration::from_secs(30),
        )
        .await?;
    eprintln!("HTTP proxy scenario: waiting for verifier deployment cleanup");
    harness
        .kube
        .wait_deleted(
            "deployment",
            &test_deployment,
            &cfg.namespace,
            Duration::from_secs(30),
        )
        .await?;
    eprintln!("HTTP proxy scenario: waiting for verifier service cleanup");
    harness
        .kube
        .wait_deleted(
            "service",
            &test_deployment,
            &cfg.namespace,
            Duration::from_secs(30),
        )
        .await?;
    eprintln!("HTTP proxy scenario: waiting for inception resource cleanup");
    harness
        .kube
        .wait_no_inception_resources(&cfg.namespace)
        .await?;
    eprintln!("HTTP proxy scenario: waiting for promoted deployment labels and replicas");
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

async fn verify_http_plugin_proxy_and_mock(
    harness: &E2eHarness,
    proxy_plugin: &str,
    test_deployment: &str,
) -> Result<()> {
    let proxy_selector = harness
        .kube
        .deployment_pod_selector(proxy_plugin, &harness.config.namespace)
        .await?;
    let proxy_case = format!("http-direct-proxy-{}", unique_token("case"));
    let response = harness
        .kube
        .pod_http_by_selector(
            &harness.config.namespace,
            &proxy_selector,
            9090,
            PodHttpRequest {
                method: "POST",
                path: "/?via=fluidbg",
                basic_auth: None,
                headers: vec![("X-FluidBG-E2E", "proxy")],
                body: Some(serde_json::json!({
                    "orderId": proxy_case,
                    "action": "http-proxy-check"
                })),
            },
        )
        .await?;
    if !(200..300).contains(&response.status) {
        bail!(
            "expected direct HTTP proxy call to succeed, got {}",
            response.status
        );
    }
    let upstream: serde_json::Value =
        serde_json::from_slice(&response.body).unwrap_or(serde_json::Value::Null);
    if upstream
        .pointer("/headers/X-Fluidbg-E2E")
        .and_then(|v| v.as_str())
        != Some("proxy")
    {
        bail!("HTTP proxy did not forward request headers to upstream: {upstream}");
    }
    if upstream.pointer("/args/via").and_then(|v| v.as_str()) != Some("fluidbg") {
        bail!("HTTP proxy did not preserve query string to upstream: {upstream}");
    }

    let mock_case = format!("http-mock-{}", unique_token("case"));
    let response = harness
        .kube
        .pod_http_by_selector(
            &harness.config.namespace,
            &proxy_selector,
            9090,
            PodHttpRequest {
                method: "POST",
                path: "/mocked",
                basic_auth: None,
                headers: vec![("X-FluidBG-E2E", "mock")],
                body: Some(serde_json::json!({
                    "orderId": mock_case,
                    "action": "http-mock-check"
                })),
            },
        )
        .await?;
    if response.status != 209 {
        bail!(
            "expected HTTP mock verifier response status 209, got {}",
            response.status
        );
    }
    let mock_body: serde_json::Value = serde_json::from_slice(&response.body)?;
    if mock_body.get("mocked").and_then(|v| v.as_bool()) != Some(true) {
        bail!("HTTP mock response was not returned from verifier: {mock_body}");
    }

    let test_selector = harness
        .kube
        .deployment_pod_selector(test_deployment, &harness.config.namespace)
        .await?;
    for _ in 1..=30 {
        let cases = harness
            .kube
            .pod_http_json_by_selector(
                &harness.config.namespace,
                &test_selector,
                8080,
                PodHttpRequest {
                    method: "GET",
                    path: "/cases",
                    basic_auth: None,
                    headers: Vec::new(),
                    body: None,
                },
            )
            .await?;
        if let Ok(flags) = testcase_flags(&cases, &mock_case)
            && flags.mock_call_seen
            && flags.observation_seen
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    bail!("expected HTTP mock call and observer notification to be visible in verifier")
}
