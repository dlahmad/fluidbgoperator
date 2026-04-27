use std::time::Duration;

use anyhow::Result;
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use fluidbg_plugin_sdk::{
    AUTHORIZATION_HEADER, PluginDrainStatusResponse, PluginLifecycleResponse, PluginRole,
    TrafficShiftRequest, TrafficShiftResponse, bearer_matches,
};

use crate::amqp::{connect_with_retry, declare_queue, delete_queue, queue_state};
use crate::assignments::{build_drain_assignments, build_prepare_assignments};
use crate::config::{
    AppState, RuntimeMode, combiner_config, duplicator_config, has_role, inceptor_infra_disabled,
    required, shadow_queue_name, splitter_config, writer_config,
};
use crate::management::{ManagementClient, QueueDepth};

pub(crate) async fn compute_drain_status(state: &AppState) -> Result<PluginDrainStatusResponse> {
    let conn = connect_with_retry(&state.amqp_url).await?;
    let channel = conn.create_channel().await?;

    if has_role(&state.roles, PluginRole::Duplicator) {
        let config = duplicator_config(&state.config)?;
        return input_drain_status(
            state,
            &channel,
            required(&config.green_input_queue, "duplicator.greenInputQueue")?,
            required(&config.blue_input_queue, "duplicator.blueInputQueue")?,
        )
        .await;
    }

    if has_role(&state.roles, PluginRole::Splitter) {
        let config = splitter_config(&state.config)?;
        return input_drain_status(
            state,
            &channel,
            required(&config.green_input_queue, "splitter.greenInputQueue")?,
            required(&config.blue_input_queue, "splitter.blueInputQueue")?,
        )
        .await;
    }

    if has_role(&state.roles, PluginRole::Combiner) {
        let config = combiner_config(&state.config)?;
        let green_queue = required(&config.green_output_queue, "combiner.greenOutputQueue")?;
        let blue_queue = required(&config.blue_output_queue, "combiner.blueOutputQueue")?;
        return queue_pair_drain_status(
            state,
            &channel,
            green_queue,
            blue_queue,
            "temporary output queues",
        )
        .await;
    }

    Ok(PluginDrainStatusResponse {
        drained: true,
        message: Some("no drain-sensitive RabbitMQ roles active".to_string()),
    })
}

async fn input_drain_status(
    state: &AppState,
    channel: &lapin::Channel,
    green_queue: &str,
    blue_queue: &str,
) -> Result<PluginDrainStatusResponse> {
    queue_pair_drain_status(
        state,
        channel,
        green_queue,
        blue_queue,
        "temporary input queues",
    )
    .await
}

async fn queue_pair_drain_status(
    state: &AppState,
    channel: &lapin::Channel,
    green_queue: &str,
    blue_queue: &str,
    label: &str,
) -> Result<PluginDrainStatusResponse> {
    if let Some(management) = ManagementClient::from_config(&state.config) {
        let green = management_queue_depth_with_shadow(&management, state, green_queue).await?;
        let blue = management_queue_depth_with_shadow(&management, state, blue_queue).await?;
        return Ok(management_drain_status(label, &green, &blue));
    }

    let (green_ready, green_consumers) =
        queue_state_with_shadow(channel, state, green_queue).await?;
    let (blue_ready, blue_consumers) = queue_state_with_shadow(channel, state, blue_queue).await?;
    let drained = green_ready == 0 && blue_ready == 0;
    let message = if drained {
        Some(format!(
            "{label} have no ready messages; RabbitMQ management API is not configured, so unacknowledged counts are not directly observable (consumers: green={green_consumers}, blue={blue_consumers})"
        ))
    } else {
        Some(format!(
            "{label} still contain ready messages (green={green_ready}, blue={blue_ready})"
        ))
    };
    Ok(PluginDrainStatusResponse { drained, message })
}

async fn management_queue_depth_with_shadow(
    management: &ManagementClient,
    state: &AppState,
    queue: &str,
) -> Result<QueueDepth> {
    let mut depth = management.queue_depth(queue).await?;
    if let Some(shadow_queue) = shadow_queue_name(&state.config, queue) {
        let shadow = management.queue_depth(&shadow_queue).await?;
        depth.ready += shadow.ready;
        depth.unacknowledged += shadow.unacknowledged;
        depth.consumers += shadow.consumers;
    }
    Ok(depth)
}

async fn queue_state_with_shadow(
    channel: &lapin::Channel,
    state: &AppState,
    queue: &str,
) -> Result<(u32, u32)> {
    let (mut ready, mut consumers) = queue_state(channel, queue).await?;
    if let Some(shadow_queue) = shadow_queue_name(&state.config, queue) {
        let (shadow_ready, shadow_consumers) = queue_state(channel, &shadow_queue).await?;
        ready += shadow_ready;
        consumers += shadow_consumers;
    }
    Ok((ready, consumers))
}

fn management_drain_status(
    label: &str,
    green: &QueueDepth,
    blue: &QueueDepth,
) -> PluginDrainStatusResponse {
    let green_total = green.ready + green.unacknowledged;
    let blue_total = blue.ready + blue.unacknowledged;
    let drained = green_total == 0 && blue_total == 0;
    let message = if drained {
        Some(format!(
            "{label} are empty and have no unacknowledged messages (consumers: green={}, blue={})",
            green.consumers, blue.consumers
        ))
    } else {
        Some(format!(
            "{label} not drained (green ready={}, unacked={}, consumers={}; blue ready={}, unacked={}, consumers={})",
            green.ready,
            green.unacknowledged,
            green.consumers,
            blue.ready,
            blue.unacknowledged,
            blue.consumers
        ))
    };
    PluginDrainStatusResponse { drained, message }
}

pub(crate) async fn prepare_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<PluginLifecycleResponse>, axum::http::StatusCode> {
    authorize_operator(&state, &headers)?;
    state.set_runtime_mode(RuntimeMode::Active);
    if inceptor_infra_disabled() {
        return Ok(Json(PluginLifecycleResponse {
            assignments: build_prepare_assignments(&state.config, &state.roles),
        }));
    }
    let conn = connect_with_retry(&state.amqp_url)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    let channel = conn
        .create_channel()
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    if has_role(&state.roles, PluginRole::Duplicator) {
        let duplicator = duplicator_config(&state.config)
            .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
        declare_temp_queues(
            &state,
            &channel,
            &[
                duplicator.green_input_queue.as_deref(),
                duplicator.blue_input_queue.as_deref(),
            ],
        )
        .await?;
    }
    if has_role(&state.roles, PluginRole::Splitter) {
        let splitter = splitter_config(&state.config)
            .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
        declare_temp_queues(
            &state,
            &channel,
            &[
                splitter.green_input_queue.as_deref(),
                splitter.blue_input_queue.as_deref(),
            ],
        )
        .await?;
    }
    if has_role(&state.roles, PluginRole::Combiner) {
        let combiner = combiner_config(&state.config)
            .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
        declare_temp_queues(
            &state,
            &channel,
            &[
                combiner.green_output_queue.as_deref(),
                combiner.blue_output_queue.as_deref(),
                combiner.output_queue.as_deref(),
            ],
        )
        .await?;
    }
    if has_role(&state.roles, PluginRole::Writer) {
        let writer = writer_config(&state.config)
            .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
        declare_temp_queues(&state, &channel, &[writer.target_queue.as_deref()]).await?;
    }

    Ok(Json(PluginLifecycleResponse {
        assignments: build_prepare_assignments(&state.config, &state.roles),
    }))
}

pub(crate) async fn cleanup_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<PluginLifecycleResponse>, axum::http::StatusCode> {
    authorize_operator(&state, &headers)?;
    state.set_runtime_mode(RuntimeMode::Idle);
    if inceptor_infra_disabled() {
        return Ok(Json(PluginLifecycleResponse {
            assignments: Vec::new(),
        }));
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    let conn = connect_with_retry(&state.amqp_url)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    let channel = conn
        .create_channel()
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    if has_role(&state.roles, PluginRole::Duplicator) {
        let duplicator = duplicator_config(&state.config)
            .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
        delete_temp_queues(
            &state,
            &channel,
            &[
                duplicator.green_input_queue.as_deref(),
                duplicator.blue_input_queue.as_deref(),
            ],
        )
        .await?;
    }
    if has_role(&state.roles, PluginRole::Splitter) {
        let splitter = splitter_config(&state.config)
            .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
        delete_temp_queues(
            &state,
            &channel,
            &[
                splitter.green_input_queue.as_deref(),
                splitter.blue_input_queue.as_deref(),
            ],
        )
        .await?;
    }
    if has_role(&state.roles, PluginRole::Combiner) {
        let combiner = combiner_config(&state.config)
            .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
        delete_temp_queues(
            &state,
            &channel,
            &[
                combiner.green_output_queue.as_deref(),
                combiner.blue_output_queue.as_deref(),
            ],
        )
        .await?;
    }

    Ok(Json(PluginLifecycleResponse {
        assignments: Vec::new(),
    }))
}

pub(crate) async fn drain_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<PluginLifecycleResponse>, axum::http::StatusCode> {
    authorize_operator(&state, &headers)?;
    state.set_runtime_mode(RuntimeMode::Draining);
    Ok(Json(PluginLifecycleResponse {
        assignments: build_drain_assignments(&state.config, &state.roles),
    }))
}

pub(crate) async fn drain_status_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<PluginDrainStatusResponse>, axum::http::StatusCode> {
    authorize_operator(&state, &headers)?;
    let status = compute_drain_status(&state)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(status))
}

pub(crate) async fn traffic_shift_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<TrafficShiftRequest>,
) -> Result<Json<TrafficShiftResponse>, axum::http::StatusCode> {
    authorize_operator(&state, &headers)?;
    state.set_traffic_percent(req.traffic_percent);
    Ok(Json(TrafficShiftResponse {
        traffic_percent: state.traffic_percent(),
    }))
}

pub(crate) async fn health() -> &'static str {
    "ok"
}

fn authorize_operator(state: &AppState, headers: &HeaderMap) -> Result<(), StatusCode> {
    let header = headers
        .get(AUTHORIZATION_HEADER)
        .and_then(|value| value.to_str().ok());
    if bearer_matches(header, state.runtime.auth_token()) {
        Ok(())
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

async fn declare_temp_queues(
    state: &AppState,
    channel: &lapin::Channel,
    queues: &[Option<&str>],
) -> Result<(), axum::http::StatusCode> {
    for queue in queues.iter().flatten() {
        if let Some(shadow_queue) = shadow_queue_name(&state.config, queue) {
            let shadow_declaration = state
                .config
                .shadow_queue
                .as_ref()
                .map(|shadow| &shadow.queue_declaration)
                .unwrap_or(&state.config.queue_declaration);
            declare_queue(channel, &shadow_queue, shadow_declaration)
                .await
                .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
            declare_queue(channel, queue, &state.config.queue_declaration)
                .await
                .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
        } else {
            declare_queue(channel, queue, &state.config.queue_declaration)
                .await
                .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
        }
    }
    Ok(())
}

async fn delete_temp_queues(
    state: &AppState,
    channel: &lapin::Channel,
    queues: &[Option<&str>],
) -> Result<(), axum::http::StatusCode> {
    for queue in queues.iter().flatten() {
        delete_queue(channel, queue)
            .await
            .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
        if let Some(shadow_queue) = shadow_queue_name(&state.config, queue) {
            delete_queue(channel, &shadow_queue)
                .await
                .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn management_drain_ignores_attached_consumers_when_no_messages_exist() {
        let status = management_drain_status(
            "temporary queues",
            &QueueDepth {
                ready: 0,
                unacknowledged: 0,
                consumers: 2,
            },
            &QueueDepth {
                ready: 0,
                unacknowledged: 0,
                consumers: 1,
            },
        );

        assert!(status.drained);
    }

    #[test]
    fn management_drain_waits_for_unacknowledged_messages() {
        let status = management_drain_status(
            "temporary queues",
            &QueueDepth {
                ready: 0,
                unacknowledged: 1,
                consumers: 0,
            },
            &QueueDepth {
                ready: 0,
                unacknowledged: 0,
                consumers: 0,
            },
        );

        assert!(!status.drained);
    }
}
