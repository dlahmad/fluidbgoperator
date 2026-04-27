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
use crate::combiner::drain_output_queues;
use crate::config::{
    AppState, RuntimeMode, combiner_config, duplicator_config, has_role, inceptor_infra_disabled,
    required, shadow_queue_name, splitter_config, writer_config,
};
use crate::input::drain_input_queues;

pub(crate) async fn compute_drain_status(
    state: &AppState,
) -> anyhow::Result<PluginDrainStatusResponse> {
    if matches!(state.runtime_mode(), RuntimeMode::Draining) {
        drain_runtime_queues(state).await?;
    }

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
        let green_messages = queue_drain_message_count_with_shadow(state, green_queue).await?;
        let blue_messages = queue_drain_message_count_with_shadow(state, blue_queue).await?;
        let in_flight = state.in_flight_messages();
        let drained = green_messages == 0 && blue_messages == 0 && in_flight == 0;
        let message = if drained {
            Some(
                "temporary output queues report zero total messages and no plugin-owned locked messages remain"
                    .to_string(),
            )
        } else if in_flight > 0 {
            Some(format!(
                "temporary output queues still have {in_flight} plugin-owned locked message(s)"
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

async fn drain_runtime_queues(state: &AppState) -> anyhow::Result<()> {
    drain_input_queues(state).await?;
    drain_output_queues(state).await?;
    Ok(())
}

async fn input_drain_status(
    state: &AppState,
    green_queue: &str,
    blue_queue: &str,
) -> anyhow::Result<PluginDrainStatusResponse> {
    let green_messages = queue_drain_message_count_with_shadow(state, green_queue).await?;
    let blue_messages = queue_drain_message_count_with_shadow(state, blue_queue).await?;
    let in_flight = state.in_flight_messages();
    let drained = green_messages == 0 && blue_messages == 0 && in_flight == 0;
    let message = if drained {
        Some(
                "temporary input queues report zero total messages and no plugin-owned locked messages remain"
                    .to_string(),
            )
    } else if in_flight > 0 {
        Some(format!(
            "temporary input queues still have {in_flight} plugin-owned locked message(s)"
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
    drain_runtime_queues(&state)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
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
        if let Some(shadow_queue) = shadow_queue_name(&state.config, queue) {
            let shadow_declaration = state
                .config
                .shadow_queue
                .as_ref()
                .map(|shadow| &shadow.queue_declaration)
                .unwrap_or(&state.config.queue_declaration);
            state
                .service_bus
                .create_queue(&shadow_queue, shadow_declaration)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            state
                .service_bus
                .create_queue(queue, &state.config.queue_declaration)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        } else {
            state
                .service_bus
                .create_queue(queue, &state.config.queue_declaration)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        }
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
        if let Some(shadow_queue) = shadow_queue_name(&state.config, queue) {
            state
                .service_bus
                .delete_queue(&shadow_queue)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        }
    }
    Ok(())
}

async fn queue_drain_message_count_with_shadow(
    state: &AppState,
    queue: &str,
) -> anyhow::Result<u64> {
    let mut total = state.service_bus.queue_drain_message_count(queue).await?;
    if let Some(shadow_queue) = shadow_queue_name(&state.config, queue) {
        total += state
            .service_bus
            .queue_drain_message_count(&shadow_queue)
            .await?;
    }
    Ok(total)
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
