use std::time::Duration;

use anyhow::{Result, bail};

use crate::harness::E2eHarness;
use crate::status::bgd_status;

use super::support::{
    unique_token, wait_for_bgd_rollout_generation, wait_for_queue_json_field,
    wait_for_terminal_phase,
};

pub async fn force_replace_drains_before_new_generation(
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
        .apply_file(&cfg.deploy_file("force-replace/initial.yaml"))
        .await?;
    tokio::time::sleep(Duration::from_secs(5)).await;

    let initial_deployment = harness
        .kube
        .wait_bgd_generated_name("order-processor-force-replace", &cfg.namespace)
        .await?;
    let input_plugin = harness
        .kube
        .wait_inception_deployment_name(
            "order-processor-force-replace",
            "incoming-orders",
            &cfg.namespace,
        )
        .await?;
    let output_plugin = harness
        .kube
        .wait_inception_deployment_name(
            "order-processor-force-replace",
            "outgoing-results",
            &cfg.namespace,
        )
        .await?;
    let test_deployment = harness
        .kube
        .wait_test_deployment_name(
            "order-processor-force-replace",
            "test-container",
            &cfg.namespace,
        )
        .await?;
    for rollout in [
        previous_green,
        &initial_deployment,
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
            "order-processor-force-replace",
            &cfg.namespace,
            "Observing",
            60,
        )
        .await?;

    let green_input_queue = harness
        .kube
        .get_inception_config_value(
            "order-processor-force-replace",
            "incoming-orders",
            &cfg.namespace,
            "duplicator.greenInputQueue",
        )
        .await?;
    let green_output_queue = harness
        .kube
        .get_inception_config_value(
            "order-processor-force-replace",
            "outgoing-results",
            &cfg.namespace,
            "combiner.greenOutputQueue",
        )
        .await?;
    let input_shadow_token = unique_token("input-shadow-force-replace");
    let output_shadow_token = unique_token("output-shadow-force-replace");
    harness
        .rabbitmq
        .publish(
            &format!("{green_input_queue}_dlq"),
            &format!(r#"{{"shadowToken":"{input_shadow_token}","type":"order"}}"#),
        )
        .await?;
    harness
        .rabbitmq
        .publish(
            &format!("{green_output_queue}_dlq"),
            &format!(r#"{{"shadowToken":"{output_shadow_token}","type":"result"}}"#),
        )
        .await?;

    assert_no_tracked_cases(harness).await?;
    harness
        .kube
        .apply_file(&cfg.deploy_file("force-replace/replacement.yaml"))
        .await?;

    wait_for_queue_json_field(
        harness,
        "orders_dlq",
        "shadowToken",
        &input_shadow_token,
        30,
    )
    .await?;
    wait_for_queue_json_field(
        harness,
        "results_dlq",
        "shadowToken",
        &output_shadow_token,
        30,
    )
    .await?;
    harness
        .kube
        .wait_deployment_label(previous_green, &cfg.namespace, "fluidbg.io/green", "true")
        .await?;

    wait_for_bgd_rollout_generation(harness, "order-processor-force-replace", 60).await?;
    let replacement_deployment =
        wait_for_replacement_generated_name(harness, &initial_deployment).await?;
    harness
        .kube
        .wait_bgd_phase(
            "order-processor-force-replace",
            &cfg.namespace,
            "Observing",
            60,
        )
        .await?;
    harness
        .kube
        .wait_deployment_replicas(&replacement_deployment, &cfg.namespace, 1)
        .await?;
    let marker = harness
        .kube
        .deployment_env_value(
            &replacement_deployment,
            &cfg.namespace,
            "FORCE_REPLACE_GENERATION",
        )
        .await?;
    if marker.as_deref() != Some("replacement") {
        bail!("replacement candidate did not use the replacement spec");
    }
    let reset_status = bgd_status(
        &harness
            .kube
            .bgd_json("order-processor-force-replace", &cfg.namespace)
            .await?,
    );
    if reset_status.tracked_cases() != 0 {
        bail!(
            "force-replace replacement inherited {} stale tracked case(s)",
            reset_status.tracked_cases()
        );
    }

    let replacement_green_input_queue = harness
        .kube
        .get_inception_config_value(
            "order-processor-force-replace",
            "incoming-orders",
            &cfg.namespace,
            "duplicator.greenInputQueue",
        )
        .await?;
    let replacement_blue_input_queue = harness
        .kube
        .get_inception_config_value(
            "order-processor-force-replace",
            "incoming-orders",
            &cfg.namespace,
            "duplicator.blueInputQueue",
        )
        .await?;
    harness
        .rabbitmq
        .wait_for_consumers(&replacement_green_input_queue, 1, Duration::from_secs(60))
        .await?;
    harness
        .rabbitmq
        .wait_for_consumers(&replacement_blue_input_queue, 1, Duration::from_secs(60))
        .await?;

    for i in 1..=8 {
        harness
            .rabbitmq
            .publish(
                "orders",
                &format!(
                    r#"{{"orderId":"force-replace-final-{i}","type":"order","action":"force-replace-check"}}"#
                ),
            )
            .await?;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    wait_for_terminal_phase(harness, "order-processor-force-replace", "Completed", 60).await?;
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
        .wait_no_inception_resources(&cfg.namespace)
        .await?;
    harness
        .kube
        .wait_deployment_label(
            &replacement_deployment,
            &cfg.namespace,
            "fluidbg.io/green",
            "true",
        )
        .await?;
    Ok(replacement_deployment)
}

async fn assert_no_tracked_cases(harness: &E2eHarness) -> Result<()> {
    let status = bgd_status(
        &harness
            .kube
            .bgd_json("order-processor-force-replace", &harness.config.namespace)
            .await?,
    );
    if status.tracked_cases() == 0 {
        Ok(())
    } else {
        bail!(
            "force-replace early-update precondition failed: initial rollout already had {} tracked case(s)",
            status.tracked_cases()
        )
    }
}

async fn wait_for_replacement_generated_name(
    harness: &E2eHarness,
    initial_deployment: &str,
) -> Result<String> {
    for _ in 1..=60 {
        let deployment = harness
            .kube
            .wait_bgd_generated_name("order-processor-force-replace", &harness.config.namespace)
            .await?;
        if deployment != initial_deployment {
            return Ok(deployment);
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    bail!("force-replace BGD did not generate a replacement candidate name")
}
