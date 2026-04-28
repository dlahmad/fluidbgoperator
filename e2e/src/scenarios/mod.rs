use std::time::Duration;

use anyhow::{Result, bail};
use serde_json::Value;

use crate::command;
use crate::config::StateStore;
use crate::harness::E2eHarness;
use crate::kube::EnvPairExpectation;
use crate::status::{bgd_status, condition_status, ensure_no_drain_timeouts, testcase_flags};

pub async fn run_full_suite(harness: &mut E2eHarness) -> Result<()> {
    let bootstrap = bootstrap_initial_green(harness).await?;
    let promoted = successful_rabbitmq_promotion(harness, &bootstrap).await?;
    rollback_recovers_temporary_and_shadow_queues(harness, &promoted).await?;
    progressive_support_is_enforced(harness).await?;
    let progressive = progressive_splitter_promotion(harness, &promoted).await?;
    let http = http_proxy_observer_promotion(harness, &progressive).await?;
    force_deleted_bgd_is_cleaned_as_orphan(harness, &http).await?;
    helm_uninstall_cleans_operator_resources(harness).await
}

async fn bootstrap_initial_green(harness: &E2eHarness) -> Result<String> {
    let cfg = harness.config.clone();
    harness
        .kube
        .apply_file(&cfg.deploy_file("05-bootstrap-bgd.yaml"))
        .await?;
    let deployment = harness
        .kube
        .wait_bgd_generated_name("order-processor-bootstrap", &cfg.namespace)
        .await?;
    harness
        .kube
        .wait_bgd_phase("order-processor-bootstrap", &cfg.namespace, "Completed", 30)
        .await?;
    harness
        .kube
        .wait_exists(
            "deployment",
            &deployment,
            &cfg.namespace,
            Duration::from_secs(60),
        )
        .await?;
    harness
        .kube
        .rollout_status(&deployment, &cfg.namespace, Duration::from_secs(120))
        .await?;
    harness
        .kube
        .wait_deployment_label(&deployment, &cfg.namespace, "fluidbg.io/green", "true")
        .await?;
    Ok(deployment)
}

async fn successful_rabbitmq_promotion(
    harness: &mut E2eHarness,
    bootstrap_deployment: &str,
) -> Result<String> {
    let cfg = harness.config.clone();
    harness
        .kube
        .apply_file(&cfg.deploy_file("06-upgrade-bgd.yaml"))
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

async fn rollback_recovers_temporary_and_shadow_queues(
    harness: &mut E2eHarness,
    current_green: &str,
) -> Result<()> {
    let cfg = harness.config.clone();
    harness
        .kube
        .apply_file(&cfg.deploy_file("07-failing-upgrade-bgd.yaml"))
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

async fn progressive_support_is_enforced(harness: &E2eHarness) -> Result<()> {
    let cfg = harness.config.clone();
    harness
        .kube
        .wait_no_inception_resources(&cfg.namespace)
        .await?;
    harness
        .kube
        .apply_file(&cfg.deploy_file("08-progressive-unsupported.yaml"))
        .await?;
    let deployment = harness
        .kube
        .wait_bgd_generated_name("order-processor-progressive-unsupported", &cfg.namespace)
        .await?;
    tokio::time::sleep(Duration::from_secs(8)).await;
    if harness
        .kube
        .exists("deployment", &deployment, &cfg.namespace)
        .await
    {
        bail!("unsupported progressive plugin created candidate deployment {deployment}");
    }
    let phase = harness
        .kube
        .bgd("order-processor-progressive-unsupported", &cfg.namespace)
        .await
        .ok()
        .and_then(|bgd| bgd.status.and_then(|status| status.phase))
        .map(|phase| format!("{phase:?}"))
        .unwrap_or_default();
    if phase == "Observing" || phase == "Completed" {
        bail!("unsupported progressive plugin reached unexpected phase {phase}");
    }
    harness
        .kube
        .delete_named(
            "bluegreendeployment",
            "order-processor-progressive-unsupported",
            &cfg.namespace,
        )
        .await?;
    harness
        .kube
        .delete_named("inceptionplugin", "rabbitmq-no-progressive", &cfg.namespace)
        .await?;
    harness
        .kube
        .wait_deleted(
            "bluegreendeployment",
            "order-processor-progressive-unsupported",
            &cfg.namespace,
            Duration::from_secs(30),
        )
        .await?;
    harness
        .kube
        .wait_deleted(
            "inceptionplugin",
            "rabbitmq-no-progressive",
            &cfg.namespace,
            Duration::from_secs(30),
        )
        .await?;
    harness
        .kube
        .wait_no_inception_resources(&cfg.namespace)
        .await
}

async fn progressive_splitter_promotion(
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
        .apply_file(&cfg.deploy_file("09-progressive-upgrade-bgd.yaml"))
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

    let target_published = 120;
    let mut published = 0;
    let mut live_shift_verified = false;
    for _ in 1..=180 {
        let status_json = harness
            .kube
            .bgd_json("order-processor-progressive-upgrade", &cfg.namespace)
            .await?;
        let status = bgd_status(&status_json);
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
            while published < target_published {
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
    if status.test_cases_observed >= target_published {
        bail!(
            "expected green-routed progressive messages to be skipped; observed={} published={target_published}",
            status.test_cases_observed
        );
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

async fn http_proxy_observer_promotion(
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
        .apply_file(&cfg.deploy_file("10-http-plugin-bgd.yaml"))
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
    let mut published_test_ids = Vec::new();
    for attempt in 1..=30 {
        let test_id = format!("http-proxy-{}-{attempt}", unique_token("case"));
        harness
            .rabbitmq
            .publish(
                "orders",
                &format!(r#"{{"orderId":"{test_id}","type":"order","action":"http-proxy-check"}}"#),
            )
            .await?;
        published_test_ids.push(test_id);

        let status = bgd_status(
            &harness
                .kube
                .bgd_json("order-processor-http-upgrade", &cfg.namespace)
                .await?,
        );
        if status.tracked_cases() >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    let mut verified = false;
    for _ in 1..=60 {
        let status_json = harness
            .kube
            .bgd_json("order-processor-http-upgrade", &cfg.namespace)
            .await?;
        let status = bgd_status(&status_json);
        if status.test_cases_passed >= 1 && !verified {
            for test_id in &published_test_ids {
                let Ok(flags) = http_case_flags(&cfg.namespace, &test_deployment, test_id) else {
                    continue;
                };
                if flags.status == "passed" && flags.output_message_seen && flags.http_call_seen {
                    verified = true;
                    break;
                }
            }
            if status.test_cases_passed >= 1 && !verified {
                bail!(
                    "HTTP proxy case passed but none of the published cases had both output and HTTP observations"
                );
            }
        }
        if status.phase == "Completed" && status.test_cases_passed >= 1 {
            break;
        }
        if status.phase == "RolledBack" {
            bail!("HTTP plugin BGD rolled back");
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    let status = bgd_status(
        &harness
            .kube
            .bgd_json("order-processor-http-upgrade", &cfg.namespace)
            .await?,
    );
    if status.phase != "Completed" || status.test_cases_passed < 1 {
        bail!(
            "expected HTTP plugin BGD Completed with at least one passed test, got phase={} passed={}",
            status.phase,
            status.test_cases_passed
        );
    }
    if !verified {
        bail!("expected to verify HTTP proxy case before test deployment cleanup");
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

async fn force_deleted_bgd_is_cleaned_as_orphan(
    harness: &mut E2eHarness,
    previous_green: &str,
) -> Result<()> {
    let cfg = harness.config.clone();
    harness
        .kube
        .apply_file(&cfg.deploy_file("11-force-delete-bgd.yaml"))
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
            .postgres_case_rows(&cfg.system_namespace, "order-processor-force-delete")
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

async fn helm_uninstall_cleans_operator_resources(harness: &E2eHarness) -> Result<()> {
    let cfg = harness.config.clone();
    command::run(
        "helm",
        [
            "uninstall",
            "fluidbg-e2e",
            "-n",
            &cfg.system_namespace,
            "--wait",
        ],
    )?;
    for (resource, name, namespace) in [
        (
            "deployment",
            "fluidbg-operator",
            cfg.system_namespace.as_str(),
        ),
        (
            "deployment",
            "fluidbg-rabbitmq-manager",
            cfg.system_namespace.as_str(),
        ),
        ("service", "fluidbg-operator", cfg.system_namespace.as_str()),
        (
            "service",
            "fluidbg-rabbitmq-manager",
            cfg.system_namespace.as_str(),
        ),
        (
            "serviceaccount",
            "fluidbg-operator",
            cfg.system_namespace.as_str(),
        ),
        ("secret", "fluidbg-e2e-auth", cfg.system_namespace.as_str()),
        ("inceptionplugin", "http", cfg.namespace.as_str()),
        ("inceptionplugin", "rabbitmq", cfg.namespace.as_str()),
    ] {
        harness
            .kube
            .wait_deleted(resource, name, namespace, Duration::from_secs(60))
            .await?;
    }
    if harness
        .kube
        .exists("clusterrole", "fluidbg-operator", "")
        .await
    {
        bail!("clusterrole/fluidbg-operator was not removed by Helm uninstall");
    }
    if harness
        .kube
        .exists("clusterrolebinding", "fluidbg-operator", "")
        .await
    {
        bail!("clusterrolebinding/fluidbg-operator was not removed by Helm uninstall");
    }
    Ok(())
}

async fn wait_for_tracked_cases(
    harness: &E2eHarness,
    bgd: &str,
    min_tracked: u64,
    min_passed: u64,
    attempts: u32,
) -> Result<()> {
    for _ in 1..=attempts {
        let status = bgd_status(
            &harness
                .kube
                .bgd_json(bgd, &harness.config.namespace)
                .await?,
        );
        if status.tracked_cases() >= min_tracked && status.test_cases_passed >= min_passed {
            return Ok(());
        }
        if status.phase == "RolledBack" {
            bail!("{bgd} rolled back while waiting for observed test cases");
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    bail!("expected at least {min_tracked} tracked and {min_passed} passed test cases for {bgd}")
}

async fn wait_for_terminal_phase(
    harness: &E2eHarness,
    bgd: &str,
    expected: &str,
    attempts: u32,
) -> Result<()> {
    for _ in 1..=attempts {
        let status = bgd_status(
            &harness
                .kube
                .bgd_json(bgd, &harness.config.namespace)
                .await?,
        );
        if status.phase == expected {
            return Ok(());
        }
        if status.phase == "RolledBack" && expected != "RolledBack" {
            bail!("{bgd} rolled back");
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    bail!("{bgd} did not reach phase {expected}")
}

async fn publish_progressive_message(harness: &mut E2eHarness, index: u64) -> Result<()> {
    harness.rabbitmq.publish(
        "orders",
        &format!(
            r#"{{"orderId":"progressive-{index}","type":"progressive-order","action":"process"}}"#
        ),
    )
    .await
}

async fn wait_for_queue_json_field(
    harness: &mut E2eHarness,
    queue: &str,
    field: &str,
    expected: &str,
    attempts: u32,
) -> Result<()> {
    for _ in 1..=attempts {
        if harness
            .rabbitmq
            .queue_contains_json_field(queue, field, expected)
            .await
            .unwrap_or(false)
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    bail!("expected queue {queue} to contain {field}={expected}")
}

fn http_case_flags(
    namespace: &str,
    test_deployment: &str,
    test_id: &str,
) -> Result<crate::status::TestcaseFlags> {
    let output = command::output(
        "kubectl",
        [
            "exec",
            "-n",
            namespace,
            &format!("deploy/{test_deployment}"),
            "--",
            "python",
            "-c",
            &format!(
                "import json,urllib.request; print(json.dumps(json.load(urllib.request.urlopen('http://localhost:8080/cases')).get({test_id:?}, {{}})))"
            ),
        ],
    )?;
    let document: Value = serde_json::from_str(&format!(r#"{{"{test_id}":{output}}}"#))?;
    testcase_flags(&document, test_id)
}

fn assert_condition(document: &Value, bgd: &str, condition: &str, expected: &str) -> Result<()> {
    let actual = condition_status(document, condition).unwrap_or_default();
    if actual == expected {
        Ok(())
    } else {
        bail!("expected bluegreendeployment/{bgd} condition {condition}={expected}, got {actual}")
    }
}

fn unique_token(prefix: &str) -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    format!("{prefix}-{millis}")
}
