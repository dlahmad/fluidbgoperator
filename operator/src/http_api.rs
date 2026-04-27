use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use chrono::{Duration, Utc};
use fluidbg_plugin_sdk::{AUTHORIZATION_HEADER, RegisterTestCaseRequest};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::controller::{AuthConfig, validate_plugin_auth};
use crate::state_store::{Counts, StateStore, TestCaseRecord, TestStatus, VerificationMode};

#[derive(Clone)]
pub struct ApiState {
    store: Arc<dyn StateStore>,
    client: kube::Client,
    namespace: String,
    auth: AuthConfig,
}

#[derive(Debug, Serialize)]
pub struct RegisterTestCaseResponse {
    pub test_id: String,
    pub status: String,
}

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: String,
}

#[derive(Debug, Deserialize)]
pub struct VerdictRequest {
    pub test_id: String,
    pub passed: bool,
}

#[derive(Debug, Serialize)]
pub struct VerdictResponse {
    pub test_id: String,
    pub status: String,
}

#[derive(Debug, Serialize)]
pub struct CountsResponse {
    pub blue_green_ref: String,
    pub passed: i64,
    pub failed: i64,
    pub timed_out: i64,
    pub pending: i64,
}

pub async fn health() -> impl IntoResponse {
    Json(HealthResponse {
        status: "ok".to_string(),
    })
}

pub async fn register_test_case(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(req): Json<RegisterTestCaseRequest>,
) -> impl IntoResponse {
    info!(
        "registering testCase id={} bgd={} inceptionPoint={} verifyUrl={}",
        req.test_id,
        req.blue_green_ref,
        req.inception_point,
        req.verify_url.as_deref().unwrap_or("")
    );
    let auth_header = headers
        .get(AUTHORIZATION_HEADER)
        .and_then(|value| value.to_str().ok());
    let claims = match validate_plugin_auth(
        &state.client,
        &state.namespace,
        &state.auth,
        auth_header,
    )
    .await
    {
        Ok(Some(claims)) => claims,
        Ok(None) => {
            warn!(
                "rejecting unauthenticated testCase registration id={} bgd={} inceptionPoint={}",
                req.test_id, req.blue_green_ref, req.inception_point
            );
            return (
                StatusCode::UNAUTHORIZED,
                Json(RegisterTestCaseResponse {
                    test_id: req.test_id,
                    status: "Unauthorized".to_string(),
                }),
            );
        }
        Err(err) => {
            warn!(
                "failed to validate testCase registration auth id={} bgd={} inceptionPoint={}: {}",
                req.test_id, req.blue_green_ref, req.inception_point, err
            );
            return (
                StatusCode::UNAUTHORIZED,
                Json(RegisterTestCaseResponse {
                    test_id: req.test_id,
                    status: "Unauthorized".to_string(),
                }),
            );
        }
    };
    if !claims_match_request(&claims, &req) {
        warn!(
            "rejecting testCase registration id={} because body identity bgd={} inceptionPoint={} does not match token bgd={} inceptionPoint={}",
            req.test_id,
            req.blue_green_ref,
            req.inception_point,
            claims.blue_green_ref,
            claims.inception_point
        );
        return (
            StatusCode::UNAUTHORIZED,
            Json(RegisterTestCaseResponse {
                test_id: req.test_id,
                status: "Unauthorized".to_string(),
            }),
        );
    }
    let run = TestCaseRecord {
        test_id: req.test_id.clone(),
        blue_green_ref: req.blue_green_ref,
        triggered_at: req.triggered_at.unwrap_or_else(Utc::now),
        source_inception_point: req.inception_point,
        timeout: Duration::seconds(req.timeout_seconds.unwrap_or(60)),
        status: TestStatus::Triggered,
        verdict: None,
        verification_mode: VerificationMode::Data,
        verify_url: req.verify_url.unwrap_or_default(),
        retries_remaining: 0,
        failure_message: None,
    };
    let test_id = run.test_id.clone();
    match state.store.register(run).await {
        Ok(()) => (
            StatusCode::CREATED,
            Json(RegisterTestCaseResponse {
                test_id,
                status: "Triggered".to_string(),
            }),
        ),
        Err(e) => {
            warn!("failed to register testCase {}: {}", test_id, e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(RegisterTestCaseResponse {
                    test_id,
                    status: format!("error: {}", e),
                }),
            )
        }
    }
}

pub async fn set_verdict(
    State(state): State<ApiState>,
    Json(req): Json<VerdictRequest>,
) -> impl IntoResponse {
    info!(
        "setting verdict for testCase id={} passed={}",
        req.test_id, req.passed
    );
    match state
        .store
        .set_verdict(&req.test_id, req.passed, None)
        .await
    {
        Ok(()) => (
            StatusCode::OK,
            Json(VerdictResponse {
                test_id: req.test_id,
                status: if req.passed { "Passed" } else { "Failed" }.to_string(),
            }),
        ),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(VerdictResponse {
                test_id: req.test_id,
                status: format!("error: {}", e),
            }),
        ),
    }
}

pub async fn get_counts(
    State(state): State<ApiState>,
    axum::extract::Path(bg_ref): axum::extract::Path<String>,
) -> impl IntoResponse {
    match state.store.counts(&bg_ref).await {
        Ok(Counts {
            passed,
            failed,
            timed_out,
            pending,
        }) => (
            StatusCode::OK,
            Json(CountsResponse {
                blue_green_ref: bg_ref,
                passed,
                failed,
                timed_out,
                pending,
            }),
        ),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(CountsResponse {
                blue_green_ref: bg_ref,
                passed: 0,
                failed: 0,
                timed_out: 0,
                pending: 0,
            }),
        ),
    }
}

pub fn router(
    store: Arc<dyn StateStore>,
    client: kube::Client,
    namespace: String,
    auth: AuthConfig,
) -> axum::Router {
    let state = ApiState {
        store,
        client,
        namespace,
        auth,
    };
    axum::Router::new()
        .route("/health", get(health))
        .route("/testcases", post(register_test_case))
        .route("/testcase-verdicts", post(set_verdict))
        .route("/counts/{bg_ref}", get(get_counts))
        .with_state(state)
}

fn claims_match_request(
    claims: &fluidbg_plugin_sdk::PluginAuthClaims,
    req: &RegisterTestCaseRequest,
) -> bool {
    claims.blue_green_ref == req.blue_green_ref && claims.inception_point == req.inception_point
}

#[cfg(test)]
mod tests {
    use super::*;
    use fluidbg_plugin_sdk::PluginAuthClaims;

    fn request(blue_green_ref: &str, inception_point: &str) -> RegisterTestCaseRequest {
        RegisterTestCaseRequest {
            test_id: "case-1".to_string(),
            blue_green_ref: blue_green_ref.to_string(),
            inception_point: inception_point.to_string(),
            triggered_at: None,
            timeout_seconds: None,
            verify_url: None,
        }
    }

    #[test]
    fn auth_claims_must_match_testcase_registration_identity() {
        let claims = PluginAuthClaims::new("app", "orders", "incoming", "rabbitmq");

        assert!(claims_match_request(
            &claims,
            &request("orders", "incoming")
        ));
        assert!(!claims_match_request(
            &claims,
            &request("other", "incoming")
        ));
        assert!(!claims_match_request(
            &claims,
            &request("orders", "outgoing")
        ));
    }
}
