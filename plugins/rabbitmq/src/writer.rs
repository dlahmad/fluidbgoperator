use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use fluidbg_plugin_sdk::{AUTHORIZATION_HEADER, require_bearer_token};
use lapin::BasicProperties;
use lapin::types::FieldTable;

use crate::amqp::{connect_with_retry, publish_confirmed};
use crate::config::{AppState, WriteRequest};

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

    let conn = match connect_with_retry(&state.amqp_url).await {
        Ok(conn) => conn,
        Err(err) => {
            tracing::error!("failed to connect for write: {}", err);
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "connect failed".to_string(),
            );
        }
    };
    let channel = match conn.create_channel().await {
        Ok(channel) => channel,
        Err(err) => {
            tracing::error!("failed to create write channel: {}", err);
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "channel failed".to_string(),
            );
        }
    };
    let body = serde_json::to_vec(&req.payload).unwrap_or_default();

    let mut props = BasicProperties::default();
    if let Some(props_value) = &req.properties
        && let Some(headers) = props_value.get("headers")
        && let Ok(h) = serde_json::from_value::<FieldTable>(headers.clone())
    {
        props = props.with_headers(h);
    }

    match publish_confirmed(&channel, &queue, &body, props).await {
        Ok(_) => (axum::http::StatusCode::OK, "published".to_string()),
        Err(e) => {
            tracing::error!("failed to publish: {}", e);
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
