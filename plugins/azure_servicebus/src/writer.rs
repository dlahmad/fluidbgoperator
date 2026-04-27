use axum::{Json, extract::State};

use crate::config::{AppState, WriteRequest};
use crate::servicebus::properties_from_json;

pub(crate) async fn write_handler(
    State(state): State<AppState>,
    Json(req): Json<WriteRequest>,
) -> impl axum::response::IntoResponse {
    let Some(queue) = state
        .config
        .writer
        .as_ref()
        .and_then(|writer| writer.target_queue.clone())
    else {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            "targetQueue not configured",
        );
    };

    let body = serde_json::to_vec(&req.payload).unwrap_or_default();
    let properties = properties_from_json(&req.properties);

    match state
        .service_bus
        .send_message(&queue, &body, &properties)
        .await
    {
        Ok(_) => (axum::http::StatusCode::OK, "published"),
        Err(e) => {
            tracing::error!("failed to publish Service Bus message: {}", e);
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "publish failed",
            )
        }
    }
}
