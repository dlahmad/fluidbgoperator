use std::time::Duration;

use anyhow::Result;

use crate::harness::E2eHarness;

pub async fn namespace_deletion_does_not_deadlock_bgd_finalizers(
    harness: &E2eHarness,
) -> Result<()> {
    let cfg = harness.config.clone();
    harness
        .kube
        .delete_named("namespace", &cfg.namespace, "")
        .await?;
    harness
        .kube
        .wait_deleted("namespace", &cfg.namespace, "", Duration::from_secs(180))
        .await
}
