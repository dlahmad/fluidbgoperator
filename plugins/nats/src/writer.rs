use axum::{Json, extract::State, http::StatusCode};
use fluidbg_plugin_sdk::HttpWriteRequest;

use crate::config::{AppState, writer_config};
use crate::nats::NatsClient;

pub(crate) async fn write_handler(
    State(state): State<AppState>,
    Json(req): Json<HttpWriteRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let subject = writer_config(&state.config)
        .ok()
        .and_then(|writer| writer.target_subject.clone())
        .ok_or(StatusCode::BAD_REQUEST)?;
    let client = NatsClient::connect(&state.nats_url)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let payload = serde_json::to_vec(&req.payload).map_err(|_| StatusCode::BAD_REQUEST)?;
    client
        .publish(&subject, payload)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({"ok": true})))
}
