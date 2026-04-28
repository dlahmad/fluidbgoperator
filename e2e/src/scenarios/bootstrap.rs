use std::time::Duration;

use anyhow::Result;

use crate::harness::E2eHarness;

pub async fn bootstrap_initial_green(harness: &E2eHarness) -> Result<String> {
    let cfg = harness.config.clone();
    harness
        .kube
        .apply_file(&cfg.deploy_file("bootstrap/bgd.yaml"))
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
