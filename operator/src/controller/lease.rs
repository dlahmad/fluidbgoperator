use std::future::Future;
use std::time::Duration;

use k8s_openapi::api::coordination::v1::{Lease, LeaseSpec};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{MicroTime, ObjectMeta, OwnerReference};
use k8s_openapi::jiff::Timestamp;
use kube::api::{Api, PostParams};
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;
use tokio::time::sleep;
use tracing::{info, warn};

use super::ReconcileError;
use crate::crd::blue_green::BlueGreenDeployment;

#[derive(Clone, Debug)]
pub(super) struct LeaseConfig {
    pub holder_identity: String,
    pub duration: Duration,
    pub renew_interval: Duration,
}

pub(super) async fn run_with_bgd_lease<T, F>(
    bgd: &BlueGreenDeployment,
    client: kube::Client,
    namespace: &str,
    config: &LeaseConfig,
    future: F,
) -> std::result::Result<Option<T>, ReconcileError>
where
    F: Future<Output = std::result::Result<T, ReconcileError>>,
{
    let Some(uid) = bgd.metadata.uid.as_deref() else {
        return Ok(None);
    };
    let Some(name) = bgd.metadata.name.as_deref() else {
        return Ok(None);
    };
    let lease_name = lease_name("bgd", name, uid);
    let owner = OwnerReference {
        api_version: "fluidbg.io/v1alpha1".to_string(),
        kind: "BlueGreenDeployment".to_string(),
        name: name.to_string(),
        uid: uid.to_string(),
        block_owner_deletion: Some(false),
        controller: Some(false),
    };

    run_with_lease(client, namespace, lease_name, Some(owner), config, future).await
}

pub(super) async fn run_with_orphan_lease<T, F>(
    blue_green_ref: &str,
    client: kube::Client,
    namespace: &str,
    config: &LeaseConfig,
    future: F,
) -> std::result::Result<Option<T>, ReconcileError>
where
    F: Future<Output = std::result::Result<T, ReconcileError>>,
{
    run_with_lease(
        client,
        namespace,
        lease_name("orphan", blue_green_ref, blue_green_ref),
        None,
        config,
        future,
    )
    .await
}

async fn run_with_lease<T, F>(
    client: kube::Client,
    namespace: &str,
    lease_name: String,
    owner: Option<OwnerReference>,
    config: &LeaseConfig,
    future: F,
) -> std::result::Result<Option<T>, ReconcileError>
where
    F: Future<Output = std::result::Result<T, ReconcileError>>,
{
    let api: Api<Lease> = Api::namespaced(client, namespace);
    if !try_acquire_lease(&api, &lease_name, owner, config).await? {
        info!(
            "lease '{}' is held by another operator; reconciliation is deferred",
            lease_name
        );
        return Ok(None);
    }

    let (stop_tx, stop_rx) = oneshot::channel();
    let (lost_tx, mut lost_rx) = oneshot::channel();
    let renewal = tokio::spawn(renew_until_stopped(
        api,
        lease_name.clone(),
        config.clone(),
        stop_rx,
        lost_tx,
    ));

    let result = tokio::select! {
        result = future => result.map(Some),
        _ = &mut lost_rx => Err(ReconcileError::Store(format!(
            "lost reconcile lease '{}'; aborting in-flight reconcile",
            lease_name
        ))),
    };

    let _ = stop_tx.send(());
    renewal.abort();
    result
}

async fn try_acquire_lease(
    api: &Api<Lease>,
    lease_name: &str,
    owner: Option<OwnerReference>,
    config: &LeaseConfig,
) -> std::result::Result<bool, ReconcileError> {
    match api.get(lease_name).await {
        Ok(mut lease) => {
            if !lease_can_be_acquired(&lease, config) {
                return Ok(false);
            }
            let transitions = lease
                .spec
                .as_ref()
                .and_then(|spec| spec.lease_transitions)
                .unwrap_or_default()
                + transition_increment(&lease, config);
            lease.spec = Some(lease_spec(config, transitions, true));
            api.replace(lease_name, &PostParams::default(), &lease)
                .await
                .map(|_| true)
                .or_else(conflict_means_not_acquired)
        }
        Err(kube::Error::Api(error)) if error.code == 404 => {
            let lease = Lease {
                metadata: ObjectMeta {
                    name: Some(lease_name.to_string()),
                    owner_references: owner.map(|owner| vec![owner]),
                    ..Default::default()
                },
                spec: Some(lease_spec(config, 0, true)),
            };
            api.create(&PostParams::default(), &lease)
                .await
                .map(|_| true)
                .or_else(conflict_means_not_acquired)
        }
        Err(error) => Err(ReconcileError::K8s(error)),
    }
}

async fn renew_until_stopped(
    api: Api<Lease>,
    lease_name: String,
    config: LeaseConfig,
    mut stop_rx: oneshot::Receiver<()>,
    lost_tx: oneshot::Sender<()>,
) {
    let mut lost_tx = Some(lost_tx);
    loop {
        tokio::select! {
            _ = &mut stop_rx => return,
            _ = sleep(config.renew_interval) => {
                if let Err(error) = renew_lease(&api, &lease_name, &config).await {
                    warn!("failed to renew reconcile lease '{}': {}", lease_name, error);
                    if let Some(sender) = lost_tx.take() {
                        let _ = sender.send(());
                    }
                    return;
                }
            }
        }
    }
}

async fn renew_lease(
    api: &Api<Lease>,
    lease_name: &str,
    config: &LeaseConfig,
) -> std::result::Result<(), ReconcileError> {
    let mut lease = api.get(lease_name).await?;
    if lease
        .spec
        .as_ref()
        .and_then(|spec| spec.holder_identity.as_deref())
        != Some(config.holder_identity.as_str())
    {
        return Err(ReconcileError::Store(format!(
            "lease '{lease_name}' is no longer held by this operator"
        )));
    }
    let transitions = lease
        .spec
        .as_ref()
        .and_then(|spec| spec.lease_transitions)
        .unwrap_or_default();
    lease.spec = Some(lease_spec(config, transitions, false));
    api.replace(lease_name, &PostParams::default(), &lease)
        .await?;
    Ok(())
}

fn lease_can_be_acquired(lease: &Lease, config: &LeaseConfig) -> bool {
    let Some(spec) = lease.spec.as_ref() else {
        return true;
    };
    if spec.holder_identity.as_deref() == Some(config.holder_identity.as_str()) {
        return true;
    }
    lease_expired(spec)
}

fn lease_expired(spec: &LeaseSpec) -> bool {
    let Some(renew_time) = spec.renew_time.as_ref().or(spec.acquire_time.as_ref()) else {
        return true;
    };
    let duration_seconds = spec.lease_duration_seconds.unwrap_or_default();
    if duration_seconds <= 0 {
        return true;
    }
    Timestamp::now().duration_since(renew_time.0).as_secs_f64() >= f64::from(duration_seconds)
}

fn transition_increment(lease: &Lease, config: &LeaseConfig) -> i32 {
    if lease
        .spec
        .as_ref()
        .and_then(|spec| spec.holder_identity.as_deref())
        == Some(config.holder_identity.as_str())
    {
        0
    } else {
        1
    }
}

fn lease_spec(config: &LeaseConfig, transitions: i32, acquired: bool) -> LeaseSpec {
    let now = MicroTime(Timestamp::now());
    LeaseSpec {
        acquire_time: acquired.then_some(now.clone()),
        holder_identity: Some(config.holder_identity.clone()),
        lease_duration_seconds: Some(duration_seconds(config.duration)),
        lease_transitions: Some(transitions),
        renew_time: Some(now),
        ..Default::default()
    }
}

fn duration_seconds(duration: Duration) -> i32 {
    i32::try_from(duration.as_secs()).unwrap_or(i32::MAX).max(1)
}

fn conflict_means_not_acquired(error: kube::Error) -> std::result::Result<bool, ReconcileError> {
    match error {
        kube::Error::Api(api_error) if api_error.code == 409 => Ok(false),
        other => Err(ReconcileError::K8s(other)),
    }
}

fn lease_name(prefix: &str, name: &str, discriminator: &str) -> String {
    let suffix = short_hash(discriminator);
    let max_name_len = 63usize.saturating_sub(prefix.len() + suffix.len() + 2);
    let normalized = dns_label_prefix(name, max_name_len);
    format!("{prefix}-{normalized}-{suffix}")
}

fn dns_label_prefix(value: &str, max_len: usize) -> String {
    let mut output = String::new();
    for ch in value.chars() {
        if output.len() >= max_len {
            break;
        }
        let mapped = if ch.is_ascii_alphanumeric() {
            ch.to_ascii_lowercase()
        } else {
            '-'
        };
        if mapped == '-' && output.ends_with('-') {
            continue;
        }
        output.push(mapped);
    }
    let trimmed = output.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "bgd".to_string()
    } else {
        trimmed
    }
}

fn short_hash(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    digest
        .iter()
        .take(5)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{dns_label_prefix, lease_expired, lease_name};
    use k8s_openapi::api::coordination::v1::LeaseSpec;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime;
    use k8s_openapi::jiff::{SignedDuration, Timestamp};

    #[test]
    fn lease_name_is_dns_safe_and_bounded() {
        let name = lease_name(
            "bgd",
            "My.BGD_with_long_suffix_that_should_be_cut",
            "uid-123",
        );
        assert!(name.len() <= 63);
        assert!(
            name.chars()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
        );
    }

    #[test]
    fn dns_label_prefix_falls_back_when_empty() {
        assert_eq!(dns_label_prefix("___", 20), "bgd");
    }

    #[test]
    fn expired_lease_can_be_taken_over() {
        let spec = LeaseSpec {
            renew_time: Some(MicroTime(Timestamp::now() - SignedDuration::from_secs(20))),
            lease_duration_seconds: Some(10),
            ..Default::default()
        };
        assert!(lease_expired(&spec));
    }
}
