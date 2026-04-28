use std::time::Duration;

use anyhow::{Result, bail};

use crate::harness::E2eHarness;

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
