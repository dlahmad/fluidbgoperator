use std::sync::Arc;

use chrono::Utc;
use reqwest::Client;
use tokio::time;
use tracing::{debug, error, info, warn};

use crate::state_store::{StateStore, VerificationMode};

pub struct InceptionTracker {
    store: Arc<dyn StateStore>,
    http: Client,
    poll_interval: time::Duration,
    timeout_check_interval: time::Duration,
}

const VERIFIER_POLL_TIMEOUT_ENV: &str = "FLUIDBG_VERIFIER_POLL_TIMEOUT_SECONDS";
const VERIFIER_POLL_CONNECT_TIMEOUT_ENV: &str = "FLUIDBG_VERIFIER_POLL_CONNECT_TIMEOUT_SECONDS";
const DEFAULT_VERIFIER_POLL_TIMEOUT_SECONDS: u64 = 5;
const DEFAULT_VERIFIER_POLL_CONNECT_TIMEOUT_SECONDS: u64 = 3;

impl InceptionTracker {
    pub fn new(
        store: Arc<dyn StateStore>,
        poll_interval: time::Duration,
        timeout_check_interval: time::Duration,
    ) -> Self {
        Self {
            store,
            http: verifier_http_client(),
            poll_interval,
            timeout_check_interval,
        }
    }

    pub async fn run(&self) {
        let mut poll_tick = time::interval(self.poll_interval);
        let mut timeout_tick = time::interval(self.timeout_check_interval);

        loop {
            tokio::select! {
                _ = poll_tick.tick() => {
                    if let Err(e) = self.poll_pending().await {
                        error!("error polling pending cases: {}", e);
                    }
                }
                _ = timeout_tick.tick() => {
                    if let Err(e) = self.check_timeouts().await {
                        error!("error checking timeouts: {}", e);
                    }
                }
            }
        }
    }

    async fn poll_pending(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let pending = self.store.list_pending().await?;
        for run in &pending {
            let mut request = self.http.get(&run.verify_url);
            if !run.verifier_auth_token.is_empty() {
                request = request.header(
                    fluidbg_plugin_sdk::AUTHORIZATION_HEADER,
                    fluidbg_plugin_sdk::bearer_value(&run.verifier_auth_token),
                );
            }
            match request.send().await {
                Ok(resp) => {
                    if let Ok(body) = resp.json::<TestResultResponse>().await {
                        if let Some(passed) = body.passed {
                            if let Err(e) = self
                                .store
                                .set_verdict(
                                    &run.blue_green_ref,
                                    &run.test_id,
                                    passed,
                                    body.error_message.clone(),
                                )
                                .await
                            {
                                warn!("failed to set verdict for {}: {}", run.test_id, e);
                            } else {
                                debug!(
                                    "test {} verdict: {}",
                                    run.test_id,
                                    if passed { "Passed" } else { "Failed" }
                                );
                            }
                        } else {
                            debug!("test {} still pending (null verdict)", run.test_id);
                        }
                    } else if run.verification_mode == VerificationMode::Custom {
                        match self
                            .store
                            .decrement_retries(&run.blue_green_ref, &run.test_id)
                            .await
                        {
                            Ok(Some(remaining)) => {
                                warn!(
                                    "custom test {} verification request returned unparsable response, retrying ({} left)",
                                    run.test_id, remaining
                                );
                            }
                            Ok(None) => {
                                let _ = self
                                    .store
                                    .set_verdict(
                                        &run.blue_green_ref,
                                        &run.test_id,
                                        false,
                                        Some("custom verification retries exhausted".to_string()),
                                    )
                                    .await;
                            }
                            Err(e) => warn!("failed to update retries for {}: {}", run.test_id, e),
                        }
                    }
                }
                Err(e) => {
                    debug!("failed to poll result for {}: {}", run.test_id, e);
                }
            }
        }
        Ok(())
    }

    async fn check_timeouts(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let pending = self.store.list_pending().await?;
        let now = Utc::now();
        for run in &pending {
            if run.expires_at() < now {
                info!("test {} timed out", run.test_id);
                if let Err(e) = self
                    .store
                    .mark_timed_out(&run.blue_green_ref, &run.test_id)
                    .await
                {
                    warn!("failed to mark {} as timed out: {}", run.test_id, e);
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, serde::Deserialize)]
struct TestResultResponse {
    passed: Option<bool>,
    #[serde(rename = "errorMessage")]
    error_message: Option<String>,
}

fn verifier_http_client() -> Client {
    Client::builder()
        .connect_timeout(env_duration_seconds(
            VERIFIER_POLL_CONNECT_TIMEOUT_ENV,
            DEFAULT_VERIFIER_POLL_CONNECT_TIMEOUT_SECONDS,
        ))
        .timeout(env_duration_seconds(
            VERIFIER_POLL_TIMEOUT_ENV,
            DEFAULT_VERIFIER_POLL_TIMEOUT_SECONDS,
        ))
        .build()
        .expect("verifier HTTP client configuration must be valid")
}

fn env_duration_seconds(name: &str, default_seconds: u64) -> time::Duration {
    time::Duration::from_secs(
        std::env::var(name)
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(default_seconds),
    )
}
