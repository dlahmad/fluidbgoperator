use std::time::Duration;

use anyhow::{Result, bail};

use crate::config::StateStore;
use crate::harness::E2eHarness;
use crate::kube::EnvPairExpectation;
use crate::status::bgd_status;

use super::support::unique_token;

pub async fn force_deleted_bgd_is_cleaned_as_orphan(
    harness: &mut E2eHarness,
    previous_green: &str,
) -> Result<()> {
    let cfg = harness.config.clone();
    harness
        .kube
        .apply_file(&cfg.deploy_file("force-delete/bgd.yaml"))
        .await?;
    tokio::time::sleep(Duration::from_secs(5)).await;
    let deployment = harness
        .kube
        .wait_bgd_generated_name("order-processor-force-delete", &cfg.namespace)
        .await?;
    let input_plugin = harness
        .kube
        .wait_inception_deployment_name(
            "order-processor-force-delete",
            "incoming-orders",
            &cfg.namespace,
        )
        .await?;
    let output_plugin = harness
        .kube
        .wait_inception_deployment_name(
            "order-processor-force-delete",
            "outgoing-results",
            &cfg.namespace,
        )
        .await?;
    let test_deployment = harness
        .kube
        .wait_test_deployment_name(
            "order-processor-force-delete",
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
            "order-processor-force-delete",
            &cfg.namespace,
            "Observing",
            60,
        )
        .await?;
    let blue_input = harness
        .kube
        .get_inception_config_value(
            "order-processor-force-delete",
            "incoming-orders",
            &cfg.namespace,
            "duplicator.blueInputQueue",
        )
        .await?;
    let green_input = harness
        .kube
        .get_inception_config_value(
            "order-processor-force-delete",
            "incoming-orders",
            &cfg.namespace,
            "duplicator.greenInputQueue",
        )
        .await?;
    let blue_output = harness
        .kube
        .get_inception_config_value(
            "order-processor-force-delete",
            "outgoing-results",
            &cfg.namespace,
            "combiner.blueOutputQueue",
        )
        .await?;
    let green_output = harness
        .kube
        .get_inception_config_value(
            "order-processor-force-delete",
            "outgoing-results",
            &cfg.namespace,
            "combiner.greenOutputQueue",
        )
        .await?;
    harness
        .kube
        .wait_deployment_env_pair_values(EnvPairExpectation {
            first: previous_green,
            second: &deployment,
            namespace: &cfg.namespace,
            env_name: "INPUT_QUEUE",
            expected_a: &blue_input,
            expected_b: &green_input,
            forbidden: "orders",
        })
        .await?;
    harness
        .kube
        .wait_deployment_env_pair_values(EnvPairExpectation {
            first: previous_green,
            second: &deployment,
            namespace: &cfg.namespace,
            env_name: "OUTPUT_QUEUE",
            expected_a: &blue_output,
            expected_b: &green_output,
            forbidden: "results",
        })
        .await?;

    let mut tracked = 0;
    for i in 1..=30 {
        harness.rabbitmq.publish(
            "orders",
            &format!(
                r#"{{"orderId":"force-delete-{}-{i}","type":"order","action":"force-delete-check"}}"#,
                unique_token("case")
            ),
        ).await?;
        let status = bgd_status(
            &harness
                .kube
                .bgd_json("order-processor-force-delete", &cfg.namespace)
                .await?,
        );
        tracked = status.tracked_cases();
        if tracked >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    if tracked < 1 {
        bail!("expected force-delete BGD to register at least one store record before deletion");
    }
    harness
        .kube
        .force_delete_bgd("order-processor-force-delete", &cfg.namespace)
        .await?;
    harness
        .kube
        .wait_deleted(
            "bluegreendeployment",
            "order-processor-force-delete",
            &cfg.namespace,
            Duration::from_secs(30),
        )
        .await?;
    harness
        .kube
        .wait_no_blue_green_ref_resources(&cfg.namespace, "order-processor-force-delete")
        .await?;
    if cfg.state_store == StateStore::Postgres {
        let rows = harness
            .kube
            .postgres_case_rows(
                &cfg.system_namespace,
                &cfg.namespace,
                "order-processor-force-delete",
            )
            .await?;
        if rows != "0" {
            bail!("expected Postgres rows for force-deleted BGD to be cleaned, got {rows}");
        }
    }
    harness
        .kube
        .wait_deployment_label(previous_green, &cfg.namespace, "fluidbg.io/green", "true")
        .await
}
