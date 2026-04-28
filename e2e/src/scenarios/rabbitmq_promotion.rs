use std::time::Duration;

use anyhow::Result;

use crate::harness::E2eHarness;
use crate::kube::EnvPairExpectation;

use super::support::{
    assert_condition, wait_for_queue_json_field, wait_for_terminal_phase, wait_for_tracked_cases,
};

pub async fn successful_rabbitmq_promotion(
    harness: &mut E2eHarness,
    bootstrap_deployment: &str,
) -> Result<String> {
    let cfg = harness.config.clone();
    harness
        .kube
        .apply_file(&cfg.deploy_file("rabbitmq-promotion/bgd.yaml"))
        .await?;
    tokio::time::sleep(Duration::from_secs(5)).await;

    let deployment = harness
        .kube
        .wait_bgd_generated_name("order-processor-upgrade", &cfg.namespace)
        .await?;
    let input_plugin = harness
        .kube
        .wait_inception_deployment_name(
            "order-processor-upgrade",
            "incoming-orders",
            &cfg.namespace,
        )
        .await?;
    let output_plugin = harness
        .kube
        .wait_inception_deployment_name(
            "order-processor-upgrade",
            "outgoing-results",
            &cfg.namespace,
        )
        .await?;
    let test_deployment = harness
        .kube
        .wait_test_deployment_name("order-processor-upgrade", "test-container", &cfg.namespace)
        .await?;

    for rollout in [
        bootstrap_deployment,
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
        .wait_bgd_phase("order-processor-upgrade", &cfg.namespace, "Observing", 60)
        .await?;

    let green_input_queue = harness
        .kube
        .get_inception_config_value(
            "order-processor-upgrade",
            "incoming-orders",
            &cfg.namespace,
            "duplicator.greenInputQueue",
        )
        .await?;
    let blue_input_queue = harness
        .kube
        .get_inception_config_value(
            "order-processor-upgrade",
            "incoming-orders",
            &cfg.namespace,
            "duplicator.blueInputQueue",
        )
        .await?;
    let green_output_queue = harness
        .kube
        .get_inception_config_value(
            "order-processor-upgrade",
            "outgoing-results",
            &cfg.namespace,
            "combiner.greenOutputQueue",
        )
        .await?;
    let blue_output_queue = harness
        .kube
        .get_inception_config_value(
            "order-processor-upgrade",
            "outgoing-results",
            &cfg.namespace,
            "combiner.blueOutputQueue",
        )
        .await?;

    harness
        .kube
        .wait_deployment_env_pair_values(EnvPairExpectation {
            first: bootstrap_deployment,
            second: &deployment,
            namespace: &cfg.namespace,
            env_name: "INPUT_QUEUE",
            expected_a: &blue_input_queue,
            expected_b: &green_input_queue,
            forbidden: "orders",
        })
        .await?;
    harness
        .kube
        .wait_deployment_env_pair_values(EnvPairExpectation {
            first: bootstrap_deployment,
            second: &deployment,
            namespace: &cfg.namespace,
            env_name: "OUTPUT_QUEUE",
            expected_a: &blue_output_queue,
            expected_b: &green_output_queue,
            forbidden: "results",
        })
        .await?;
    for rollout in [bootstrap_deployment, &deployment] {
        harness
            .kube
            .rollout_status(rollout, &cfg.namespace, Duration::from_secs(120))
            .await?;
    }

    for i in 1..=5 {
        harness
            .rabbitmq
            .publish(
                "orders",
                &format!(r#"{{"orderId":"order-{i}","type":"order","action":"process"}}"#),
            )
            .await?;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    wait_for_tracked_cases(harness, "order-processor-upgrade", 5, 3, 60).await?;
    wait_for_terminal_phase(harness, "order-processor-upgrade", "Completed", 30).await?;
    harness
        .kube
        .wait_deleted(
            "deployment",
            bootstrap_deployment,
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

    for queue in [
        &green_input_queue,
        &blue_input_queue,
        &green_output_queue,
        &blue_output_queue,
    ] {
        harness.rabbitmq.assert_queue_drained(queue).await?;
    }
    for i in 1..=5 {
        wait_for_queue_json_field(harness, "results", "orderId", &format!("order-{i}"), 30).await?;
    }

    let status = harness
        .kube
        .bgd_json("order-processor-upgrade", &cfg.namespace)
        .await?;
    assert_condition(&status, "order-processor-upgrade", "Ready", "True")?;
    assert_condition(&status, "order-processor-upgrade", "Progressing", "False")?;
    assert_condition(&status, "order-processor-upgrade", "Degraded", "False")?;
    Ok(deployment)
}
