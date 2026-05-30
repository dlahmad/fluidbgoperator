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

use crate::assignments::{build_drain_assignments, build_prepare_assignments};
use crate::combiner::{combiner_durable, drain_output_subjects};
use crate::config::{
    AppState, RuntimeMode, combiner_config, duplicator_config, has_role, required, splitter_config,
};
use crate::input::{drain_input_subjects, input_drain_targets};
use crate::nats::NatsClient;

pub(crate) async fn compute_drain_status(state: &AppState) -> Result<PluginDrainStatusResponse> {
    if matches!(state.runtime_mode(), RuntimeMode::Draining) {
        drain_input_subjects(state).await?;
        drain_output_subjects(state).await?;
    }
    let client = NatsClient::connect(&state.nats_url).await?;
    let targets = drain_backlog_targets(state)?;
    let mut remaining = 0;
    let mut ack_pending = 0;
    for (subject, durable) in &targets {
        let backlog = client.consumer_backlog(subject, durable).await?;
        remaining += backlog.pending;
        ack_pending += backlog.ack_pending;
    }
    Ok(PluginDrainStatusResponse {
        drained: remaining == 0 && ack_pending == 0,
        message: Some(if remaining == 0 && ack_pending == 0 {
            "temporary NATS consumers have no pending messages".to_string()
        } else {
            format!(
                "temporary NATS consumers still have {remaining} pending and {ack_pending} ack-pending message(s)"
            )
        }),
    })
}

pub(crate) async fn prepare_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<PluginLifecycleResponse>, StatusCode> {
    authorize_operator(&state, &headers)?;
    state.set_runtime_mode(RuntimeMode::Idle);
    let client = NatsClient::connect(&state.nats_url)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    for subject in managed_subjects(&state) {
        client
            .ensure_subject_stream(&subject, &state.config.stream)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    Ok(Json(PluginLifecycleResponse {
        assignments: build_prepare_assignments(&state.config, &state.roles),
        ..Default::default()
    }))
}

pub(crate) async fn activate_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    authorize_operator(&state, &headers)?;
    state.set_runtime_mode(RuntimeMode::Active);
    Ok(Json(serde_json::json!({"ok": true})))
}

pub(crate) async fn drain_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<PluginLifecycleResponse>, StatusCode> {
    authorize_operator(&state, &headers)?;
    state.set_runtime_mode(RuntimeMode::Draining);
    Ok(Json(PluginLifecycleResponse {
        assignments: build_drain_assignments(&state.config, &state.roles),
        ..Default::default()
    }))
}

pub(crate) async fn drain_status_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<PluginDrainStatusResponse>, StatusCode> {
    authorize_operator(&state, &headers)?;
    compute_drain_status(&state)
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

pub(crate) async fn cleanup_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<PluginLifecycleResponse>, StatusCode> {
    authorize_operator(&state, &headers)?;
    state.set_runtime_mode(RuntimeMode::Idle);
    let client = NatsClient::connect(&state.nats_url)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    for subject in drain_sensitive_subjects(&state).map_err(|_| StatusCode::BAD_REQUEST)? {
        client
            .delete_subject_stream(&subject)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    Ok(Json(PluginLifecycleResponse {
        assignments: build_drain_assignments(&state.config, &state.roles),
        ..Default::default()
    }))
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

fn managed_subjects(state: &AppState) -> Vec<String> {
    let mut subjects = drain_sensitive_subjects(state).unwrap_or_default();
    if has_role(&state.roles, PluginRole::Duplicator)
        && let Ok(config) = duplicator_config(&state.config)
        && let Some(subject) = &config.input_subject
    {
        subjects.push(subject.clone());
    }
    if has_role(&state.roles, PluginRole::Splitter)
        && let Ok(config) = splitter_config(&state.config)
        && let Some(subject) = &config.input_subject
    {
        subjects.push(subject.clone());
    }
    if has_role(&state.roles, PluginRole::Combiner)
        && let Ok(config) = combiner_config(&state.config)
        && let Some(subject) = &config.output_subject
    {
        subjects.push(subject.clone());
    }
    subjects.sort();
    subjects.dedup();
    subjects
}

fn drain_sensitive_subjects(state: &AppState) -> Result<Vec<String>> {
    let mut subjects = Vec::new();
    if has_role(&state.roles, PluginRole::Duplicator) {
        let config = duplicator_config(&state.config)?;
        subjects.push(
            required(&config.green_input_subject, "duplicator.greenInputSubject")?.to_string(),
        );
        subjects
            .push(required(&config.blue_input_subject, "duplicator.blueInputSubject")?.to_string());
    }
    if has_role(&state.roles, PluginRole::Splitter) {
        let config = splitter_config(&state.config)?;
        subjects
            .push(required(&config.green_input_subject, "splitter.greenInputSubject")?.to_string());
        subjects
            .push(required(&config.blue_input_subject, "splitter.blueInputSubject")?.to_string());
    }
    if has_role(&state.roles, PluginRole::Combiner) {
        let config = combiner_config(&state.config)?;
        subjects.push(
            required(&config.green_output_subject, "combiner.greenOutputSubject")?.to_string(),
        );
        subjects
            .push(required(&config.blue_output_subject, "combiner.blueOutputSubject")?.to_string());
    }
    Ok(subjects)
}

fn drain_backlog_targets(state: &AppState) -> Result<Vec<(String, String)>> {
    let mut targets = Vec::new();
    if has_role(&state.roles, PluginRole::Duplicator)
        || has_role(&state.roles, PluginRole::Splitter)
    {
        let (_base, green, green_durable, blue, blue_durable) = input_drain_targets(state)?;
        targets.push((green.to_string(), green_durable));
        targets.push((blue.to_string(), blue_durable));
    }
    if has_role(&state.roles, PluginRole::Combiner) {
        let config = combiner_config(&state.config)?;
        let green = required(&config.green_output_subject, "combiner.greenOutputSubject")?;
        let blue = required(&config.blue_output_subject, "combiner.blueOutputSubject")?;
        targets.push((green.to_string(), combiner_durable(green)));
        targets.push((blue.to_string(), combiner_durable(blue)));
    }
    Ok(targets)
}
