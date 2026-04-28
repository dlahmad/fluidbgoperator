use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use fluidbg_plugin_sdk::{AUTHORIZATION_HEADER, require_bearer_token};

use crate::config::{AppState, WriteRequest};
use crate::servicebus::properties_from_json;

pub(crate) async fn write_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<WriteRequest>,
) -> impl axum::response::IntoResponse {
    if let Err(err) = authorize_operator(&state, &headers) {
        return err;
    }

    let Some(queue) = state
        .config
        .writer
        .as_ref()
        .and_then(|writer| writer.target_queue.clone())
    else {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            "targetQueue not configured".to_string(),
        );
    };

    let body = serde_json::to_vec(&req.payload).unwrap_or_default();
    let properties = properties_from_json(&req.properties);

    match state
        .service_bus
        .send_message(&queue, &body, &properties)
        .await
    {
        Ok(_) => (axum::http::StatusCode::OK, "published".to_string()),
        Err(e) => {
            tracing::error!("failed to publish Service Bus message: {}", e);
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "publish failed".to_string(),
            )
        }
    }
}

fn authorize_operator(state: &AppState, headers: &HeaderMap) -> Result<(), (StatusCode, String)> {
    let header = headers
        .get(AUTHORIZATION_HEADER)
        .and_then(|value| value.to_str().ok());
    require_bearer_token(header, state.runtime.auth_token())
        .map_err(|_| (StatusCode::UNAUTHORIZED, "unauthorized".to_string()))
}
