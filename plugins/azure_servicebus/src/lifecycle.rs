use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use fluidbg_plugin_sdk::{
    AUTHORIZATION_HEADER, PluginDrainStatusResponse, PluginLifecycleResponse, PluginRole,
    TrafficShiftRequest, TrafficShiftResponse, bearer_matches,
};

use crate::assignments::{build_drain_assignments, build_prepare_assignments};
use crate::config::{
    AppState, RuntimeMode, combiner_config, duplicator_config, has_role, inceptor_infra_disabled,
    required, splitter_config, writer_config,
};

pub(crate) async fn compute_drain_status(
    state: &AppState,
) -> anyhow::Result<PluginDrainStatusResponse> {
    if has_role(&state.roles, PluginRole::Duplicator) {
        let config = duplicator_config(&state.config)?;
        return input_drain_status(
            state,
            required(&config.green_input_queue, "duplicator.greenInputQueue")?,
            required(&config.blue_input_queue, "duplicator.blueInputQueue")?,
        )
        .await;
    }

    if has_role(&state.roles, PluginRole::Splitter) {
        let config = splitter_config(&state.config)?;
        return input_drain_status(
            state,
            required(&config.green_input_queue, "splitter.greenInputQueue")?,
            required(&config.blue_input_queue, "splitter.blueInputQueue")?,
        )
        .await;
    }

    if has_role(&state.roles, PluginRole::Combiner) {
        let config = combiner_config(&state.config)?;
        let green_queue = required(&config.green_output_queue, "combiner.greenOutputQueue")?;
        let blue_queue = required(&config.blue_output_queue, "combiner.blueOutputQueue")?;
        let green_messages = state.service_bus.queue_message_count(green_queue).await?;
        let blue_messages = state.service_bus.queue_message_count(blue_queue).await?;
        let empty = green_messages == 0 && blue_messages == 0;
        let drained = state.stable_drained(empty).await;
        let message = if drained {
            Some("temporary output queues are empty and stable".to_string())
        } else if empty {
            Some(format!(
                "temporary output queues are empty; waiting {}s stability window for possible locked messages",
                state.config.drain_stability_seconds
            ))
        } else {
            Some(format!(
                "temporary output queues still contain messages (green={}, blue={})",
                green_messages, blue_messages
            ))
        };
        return Ok(PluginDrainStatusResponse { drained, message });
    }

    Ok(PluginDrainStatusResponse {
        drained: true,
        message: Some("no drain-sensitive Azure Service Bus roles active".to_string()),
    })
}

async fn input_drain_status(
    state: &AppState,
    green_queue: &str,
    blue_queue: &str,
) -> anyhow::Result<PluginDrainStatusResponse> {
    let green_messages = state.service_bus.queue_message_count(green_queue).await?;
    let blue_messages = state.service_bus.queue_message_count(blue_queue).await?;
    let empty = green_messages == 0 && blue_messages == 0;
    let drained = state.stable_drained(empty).await;
    let message = if drained {
        Some("temporary input queues are empty and stable".to_string())
    } else if empty {
        Some(format!(
            "temporary input queues are empty; waiting {}s stability window for possible locked messages",
            state.config.drain_stability_seconds
        ))
    } else {
        Some(format!(
            "temporary input queues still contain messages (green={}, blue={})",
            green_messages, blue_messages
        ))
    };
    Ok(PluginDrainStatusResponse { drained, message })
}

pub(crate) async fn prepare_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<PluginLifecycleResponse>, StatusCode> {
    authorize_operator(&state, &headers)?;
    state.set_runtime_mode(RuntimeMode::Active);
    if inceptor_infra_disabled() {
        return Ok(Json(PluginLifecycleResponse {
            assignments: build_prepare_assignments(&state.config, &state.roles),
        }));
    }

    if has_role(&state.roles, PluginRole::Duplicator) {
        let duplicator = duplicator_config(&state.config).map_err(|_| StatusCode::BAD_REQUEST)?;
        create_temp_queues(
            &state,
            &[
                duplicator.green_input_queue.as_deref(),
                duplicator.blue_input_queue.as_deref(),
            ],
        )
        .await?;
    }
    if has_role(&state.roles, PluginRole::Splitter) {
        let splitter = splitter_config(&state.config).map_err(|_| StatusCode::BAD_REQUEST)?;
        create_temp_queues(
            &state,
            &[
                splitter.green_input_queue.as_deref(),
                splitter.blue_input_queue.as_deref(),
            ],
        )
        .await?;
    }
    if has_role(&state.roles, PluginRole::Combiner) {
        let combiner = combiner_config(&state.config).map_err(|_| StatusCode::BAD_REQUEST)?;
        create_temp_queues(
            &state,
            &[
                combiner.green_output_queue.as_deref(),
                combiner.blue_output_queue.as_deref(),
                combiner.output_queue.as_deref(),
            ],
        )
        .await?;
    }
    if has_role(&state.roles, PluginRole::Writer) {
        let writer = writer_config(&state.config).map_err(|_| StatusCode::BAD_REQUEST)?;
        create_temp_queues(&state, &[writer.target_queue.as_deref()]).await?;
    }

    Ok(Json(PluginLifecycleResponse {
        assignments: build_prepare_assignments(&state.config, &state.roles),
    }))
}

pub(crate) async fn cleanup_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<PluginLifecycleResponse>, StatusCode> {
    authorize_operator(&state, &headers)?;
    state.set_runtime_mode(RuntimeMode::Idle);
    if inceptor_infra_disabled() {
        return Ok(Json(PluginLifecycleResponse {
            assignments: Vec::new(),
        }));
    }

    if has_role(&state.roles, PluginRole::Duplicator) {
        let duplicator = duplicator_config(&state.config).map_err(|_| StatusCode::BAD_REQUEST)?;
        delete_temp_queues(
            &state,
            &[
                duplicator.green_input_queue.as_deref(),
                duplicator.blue_input_queue.as_deref(),
            ],
        )
        .await?;
    }
    if has_role(&state.roles, PluginRole::Splitter) {
        let splitter = splitter_config(&state.config).map_err(|_| StatusCode::BAD_REQUEST)?;
        delete_temp_queues(
            &state,
            &[
                splitter.green_input_queue.as_deref(),
                splitter.blue_input_queue.as_deref(),
            ],
        )
        .await?;
    }
    if has_role(&state.roles, PluginRole::Combiner) {
        let combiner = combiner_config(&state.config).map_err(|_| StatusCode::BAD_REQUEST)?;
        delete_temp_queues(
            &state,
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
) -> Result<Json<PluginLifecycleResponse>, StatusCode> {
    authorize_operator(&state, &headers)?;
    state.set_runtime_mode(RuntimeMode::Draining);
    Ok(Json(PluginLifecycleResponse {
        assignments: build_drain_assignments(&state.config, &state.roles),
    }))
}

pub(crate) async fn drain_status_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<PluginDrainStatusResponse>, StatusCode> {
    authorize_operator(&state, &headers)?;
    let status = compute_drain_status(&state)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(status))
}

pub(crate) async fn traffic_shift_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<TrafficShiftRequest>,
) -> Result<Json<TrafficShiftResponse>, StatusCode> {
    authorize_operator(&state, &headers)?;
    state.set_traffic_percent(req.traffic_percent);
    Ok(Json(TrafficShiftResponse {
        traffic_percent: state.traffic_percent(),
    }))
}

pub(crate) async fn health() -> &'static str {
    "ok"
}

async fn create_temp_queues(state: &AppState, queues: &[Option<&str>]) -> Result<(), StatusCode> {
    for queue in queues.iter().flatten() {
        state
            .service_bus
            .create_queue(queue)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    Ok(())
}

async fn delete_temp_queues(state: &AppState, queues: &[Option<&str>]) -> Result<(), StatusCode> {
    for queue in queues.iter().flatten() {
        state
            .service_bus
            .delete_queue(queue)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    Ok(())
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
