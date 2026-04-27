use std::sync::atomic::Ordering;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use fluidbg_plugin_sdk::{
    AUTHORIZATION_HEADER, HttpWriteRequest, PluginDrainStatusResponse, PluginLifecycleResponse,
    PluginRole, TrafficRoute, TrafficShiftRequest, TrafficShiftResponse, bearer_matches,
    routes_to_blue,
};
use serde_json::Value;
use tracing::warn;

use crate::filters::{extract_test_id, has_any_filters, matches_filter, matching_filter};
use crate::state::{ActiveRequestGuard, AppState};

pub(crate) async fn proxy_handler(
    State(state): State<AppState>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let _guard = ActiveRequestGuard::new(state.active_requests.clone());
    let method = req.method().clone();
    let uri = req.uri().clone();
    let path = uri.path().to_string();
    let headers = req.headers().clone();

    let body_bytes = axum::body::to_bytes(req.into_body(), 10 * 1024 * 1024)
        .await
        .unwrap_or_default();
    let body_json: Value = serde_json::from_slice(&body_bytes).unwrap_or(Value::Null);
    let route = if state.runtime.has_role(PluginRole::Splitter) {
        if routes_to_blue(
            &body_bytes,
            state.traffic_percent.load(Ordering::SeqCst).min(100) as u8,
        ) {
            TrafficRoute::Blue
        } else {
            TrafficRoute::Green
        }
    } else {
        TrafficRoute::Blue
    };

    let matched_root_filter = matches_filter(
        &state.config.r#match,
        method.as_str(),
        &path,
        &headers,
        &body_json,
    );
    let matched_filter =
        matching_filter(&state.config, method.as_str(), &path, &headers, &body_json);
    let matched =
        matched_root_filter && (!has_any_filters(&state.config) || matched_filter.is_some());

    if matched
        && state.runtime.observes()
        && let Some(sel) = &state.config.test_id
        && let Some(test_id) = extract_test_id(sel, &body_json, &path, &headers)
    {
        if let Some(filter) = matched_filter
            && let Some(notify_path) = &filter.notify_path
            && let Err(err) = state
                .runtime
                .notify_observer(notify_path, &test_id, &body_json, route)
                .await
        {
            warn!("failed to notify test container: {}", err);
        }

        if route.should_register_case()
            && let Err(err) = state.runtime.register_test_case(&test_id).await
        {
            warn!("failed to register case: {}", err);
        }
    }

    if matched && state.runtime.mocks() {
        return (axum::http::StatusCode::OK, "mocked by fluidbg".to_string());
    }

    let Some(real_endpoint) = state.config.routed_proxy_target(route) else {
        return (
            axum::http::StatusCode::BAD_GATEWAY,
            "realEndpoint not configured".to_string(),
        );
    };

    let client = reqwest::Client::new();
    let real_url = format!("{}{}", real_endpoint.trim_end_matches('/'), path);
    let mut req_builder = client.request(method.clone(), &real_url);

    for (name, value) in headers.iter() {
        if name != "host" && name != "content-length" {
            req_builder = req_builder.header(name, value);
        }
    }
    req_builder = req_builder.body(body_bytes.to_vec());

    match req_builder.send().await {
        Ok(resp) => {
            let status = resp.status();
            let resp_body = resp.text().await.unwrap_or_default();
            (
                axum::http::StatusCode::from_u16(status.as_u16())
                    .unwrap_or(axum::http::StatusCode::OK),
                resp_body,
            )
        }
        Err(e) => {
            warn!("upstream error: {}", e);
            (
                axum::http::StatusCode::BAD_GATEWAY,
                "upstream error".to_string(),
            )
        }
    }
}

pub(crate) async fn write_handler(
    State(state): State<AppState>,
    axum::Json(req): axum::Json<HttpWriteRequest>,
) -> impl IntoResponse {
    let _guard = ActiveRequestGuard::new(state.active_requests.clone());
    let Some(target_url) = state.config.write_target() else {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            "targetUrl not configured".to_string(),
        );
    };

    let method = req.method.as_deref().unwrap_or("POST");
    let path = req.path.as_deref().unwrap_or("/");
    let url = format!("{}{}", target_url.trim_end_matches('/'), path);

    let mut builder = state
        .runtime
        .client()
        .request(method.parse().unwrap_or(reqwest::Method::POST), &url);

    if let Some(headers) = &req.headers
        && let Some(obj) = headers.as_object()
    {
        for (key, val) in obj {
            if let Some(v) = val.as_str() {
                builder = builder.header(key, v);
            }
        }
    }

    match builder.json(&req.payload).send().await {
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            (
                axum::http::StatusCode::from_u16(status.as_u16())
                    .unwrap_or(axum::http::StatusCode::OK),
                body,
            )
        }
        Err(e) => {
            tracing::error!("failed to forward request: {}", e);
            (
                axum::http::StatusCode::BAD_GATEWAY,
                "upstream error".to_string(),
            )
        }
    }
}

pub(crate) async fn health() -> &'static str {
    "ok"
}

pub(crate) async fn prepare_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<axum::Json<PluginLifecycleResponse>, StatusCode> {
    authorize_operator(&state, &headers)?;
    state.draining.store(false, Ordering::SeqCst);
    Ok(axum::Json(PluginLifecycleResponse::default()))
}

pub(crate) async fn drain_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<axum::Json<PluginLifecycleResponse>, StatusCode> {
    authorize_operator(&state, &headers)?;
    state.draining.store(true, Ordering::SeqCst);
    Ok(axum::Json(PluginLifecycleResponse::default()))
}

pub(crate) async fn cleanup_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<axum::Json<PluginLifecycleResponse>, StatusCode> {
    authorize_operator(&state, &headers)?;
    state.draining.store(true, Ordering::SeqCst);
    Ok(axum::Json(PluginLifecycleResponse::default()))
}

pub(crate) async fn drain_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<axum::Json<PluginDrainStatusResponse>, StatusCode> {
    authorize_operator(&state, &headers)?;
    let active = state.active_requests.load(Ordering::SeqCst);
    let drained = active == 0;
    let message = if drained {
        "http plugin has no active proxy/write requests".to_string()
    } else {
        format!("http plugin still has {active} active proxy/write request(s)")
    };
    Ok(axum::Json(PluginDrainStatusResponse {
        drained,
        message: Some(message),
    }))
}

pub(crate) async fn traffic_shift_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::Json(req): axum::Json<TrafficShiftRequest>,
) -> Result<axum::Json<TrafficShiftResponse>, StatusCode> {
    authorize_operator(&state, &headers)?;
    state
        .traffic_percent
        .store(req.traffic_percent.min(100) as usize, Ordering::SeqCst);
    Ok(axum::Json(TrafficShiftResponse {
        traffic_percent: state.traffic_percent.load(Ordering::SeqCst) as u8,
    }))
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
