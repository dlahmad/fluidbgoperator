use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::state_store::{Counts, InceptionTest, StateStore, TestStatus};

#[derive(Debug, Deserialize)]
pub struct RegisterCaseRequest {
    pub test_id: String,
    pub blue_green_ref: String,
    pub inception_point: String,
    pub triggered_at: Option<DateTime<Utc>>,
    pub timeout_seconds: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct RegisterCaseResponse {
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

pub async fn register_case(
    State(store): State<Arc<dyn StateStore>>,
    Json(req): Json<RegisterCaseRequest>,
) -> impl IntoResponse {
    let run = InceptionTest {
        test_id: req.test_id.clone(),
        blue_green_ref: req.blue_green_ref,
        triggered_at: req.triggered_at.unwrap_or_else(Utc::now),
        trigger_inception_point: req.inception_point,
        timeout: Duration::seconds(req.timeout_seconds.unwrap_or(60)),
        status: TestStatus::Triggered,
        verdict: None,
    };
    let test_id = run.test_id.clone();
    match store.register(run).await {
        Ok(()) => (
            StatusCode::CREATED,
            Json(RegisterCaseResponse {
                test_id,
                status: "Triggered".to_string(),
            }),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(RegisterCaseResponse {
                test_id,
                status: format!("error: {}", e),
            }),
        ),
    }
}

pub async fn set_verdict(
    State(store): State<Arc<dyn StateStore>>,
    Json(req): Json<VerdictRequest>,
) -> impl IntoResponse {
    match store.set_verdict(&req.test_id, req.passed).await {
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
    State(store): State<Arc<dyn StateStore>>,
    axum::extract::Path(bg_ref): axum::extract::Path<String>,
) -> impl IntoResponse {
    match store.counts(&bg_ref).await {
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

pub fn router(store: Arc<dyn StateStore>) -> axum::Router {
    axum::Router::new()
        .route("/health", get(health))
        .route("/cases", post(register_case))
        .route("/verdict", post(set_verdict))
        .route("/counts/{bg_ref}", get(get_counts))
        .with_state(store)
}
