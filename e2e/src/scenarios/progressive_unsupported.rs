use anyhow::{Result, bail};
use std::time::Duration;

use crate::harness::E2eHarness;
use crate::status::{bgd_status, condition_message};

use super::support::{assert_condition, assert_condition_reason, wait_for_terminal_phase};

pub async fn progressive_support_is_enforced(harness: &E2eHarness) -> Result<()> {
    let cfg = harness.config.clone();
    harness
        .kube
        .wait_no_inception_resources(&cfg.namespace)
        .await?;
    harness
        .kube
        .apply_file(&cfg.deploy_file("progressive-unsupported/bgd.yaml"))
        .await?;
    let deployment = harness
        .kube
        .wait_bgd_generated_name("order-processor-progressive-unsupported", &cfg.namespace)
        .await?;
    wait_for_terminal_phase(
        harness,
        "order-processor-progressive-unsupported",
        "Invalid",
        30,
    )
    .await?;
    if harness
        .kube
        .exists("deployment", &deployment, &cfg.namespace)
        .await
    {
        bail!("unsupported progressive plugin created candidate deployment {deployment}");
    }

    let status_document = harness
        .kube
        .bgd_json("order-processor-progressive-unsupported", &cfg.namespace)
        .await?;
    let status = bgd_status(&status_document);
    if status.phase != "Invalid" {
        bail!(
            "unsupported progressive plugin reached unexpected phase {}",
            status.phase
        );
    }
    assert_condition(
        &status_document,
        "order-processor-progressive-unsupported",
        "Ready",
        "False",
    )?;
    assert_condition(
        &status_document,
        "order-processor-progressive-unsupported",
        "Progressing",
        "False",
    )?;
    assert_condition(
        &status_document,
        "order-processor-progressive-unsupported",
        "Degraded",
        "True",
    )?;
    assert_condition(
        &status_document,
        "order-processor-progressive-unsupported",
        "ReconcileFailed",
        "True",
    )?;
    assert_condition_reason(
        &status_document,
        "order-processor-progressive-unsupported",
        "ReconcileFailed",
        "InvalidSpec",
    )?;
    let message = condition_message(&status_document, "ReconcileFailed").unwrap_or_default();
    if !message.contains("supportsProgressiveShifting=true") {
        bail!("expected InvalidSpec message to mention progressive support, got {message}");
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
