use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::state_store::{InceptionTest, StateStore, TestStatus};

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

pub fn router(store: Arc<dyn StateStore>) -> axum::Router {
    axum::Router::new()
        .route("/health", get(health))
        .route("/cases", post(register_case))
        .with_state(store)
}
