use std::sync::atomic::Ordering;

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Response, StatusCode};
use axum::response::{IntoResponse, Response as AxumResponse};
use fluidbg_plugin_sdk::{
    AUTHORIZATION_HEADER, HttpWriteRequest, ObservationNotification, PluginDrainStatusResponse,
    PluginLifecycleResponse, PluginRole, TrafficRoute, TrafficShiftRequest, TrafficShiftResponse,
    bearer_matches, bearer_value, render_path, require_bearer_token, routes_to_blue,
};
use serde_json::Value;
use tracing::warn;

use crate::config::Config;
use crate::filters::{extract_test_id, has_any_filters, matches_filter, matching_filter};
use crate::state::{ActiveRequestGuard, AppState, RuntimeMode};

pub(crate) async fn proxy_handler(
    State(state): State<AppState>,
    req: axum::extract::Request,
) -> AxumResponse {
    if !state.runtime.has_role(PluginRole::Splitter)
        && !state.runtime.has_role(PluginRole::Observer)
        && !state.runtime.has_role(PluginRole::Mock)
    {
        return plain_response(
            axum::http::StatusCode::NOT_FOUND,
            "fluidbg http proxy role is not active",
        );
    }

    let Some(_guard) =
        ActiveRequestGuard::try_new(state.mode.clone(), state.active_requests.clone())
    else {
        return plain_response(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "fluidbg http plugin is not active",
        );
    };
    let method = req.method().clone();
    let uri = req.uri().clone();
    let path_and_query = uri
        .path_and_query()
        .map(|value| value.as_str())
        .unwrap_or("/")
        .to_string();
    let headers = req.headers().clone();

    let requires_buffer = request_requires_buffer(&state);
    if !requires_buffer {
        let matched =
            matches_without_body(&state.config, method.as_str(), &path_and_query, &headers);
        let matched_filter = matching_filter(
            &state.config,
            method.as_str(),
            &path_and_query,
            &headers,
            &Value::Null,
        );
        if matched
            && state.runtime.has_role(PluginRole::Mock)
            && should_mock_request(&state.config, matched_filter)
        {
            return forward_mock_streaming_request(
                &state,
                &method,
                &path_and_query,
                &headers,
                req.into_body(),
            )
            .await;
        }
        let Some(real_endpoint) = state.config.routed_proxy_target(TrafficRoute::Blue) else {
            return plain_response(
                axum::http::StatusCode::BAD_GATEWAY,
                "realEndpoint not configured",
            );
        };
        let real_url = join_target_url(&real_endpoint, &path_and_query);
        return forward_streaming_request(
            &state.client,
            &method,
            &real_url,
            &headers,
            req.into_body(),
            Vec::new(),
        )
        .await;
    }

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
        &path_and_query,
        &headers,
        &body_json,
    );
    let matched_filter = matching_filter(
        &state.config,
        method.as_str(),
        &path_and_query,
        &headers,
        &body_json,
    );
    let matched =
        matched_root_filter && (!has_any_filters(&state.config) || matched_filter.is_some());

    if matched
        && state.runtime.has_role(PluginRole::Observer)
        && let Some(sel) = &state.config.test_id
        && let Some(test_id) = extract_test_id(sel, &body_json, &path_and_query, &headers)
    {
        let mut verifier_notified = true;
        if let Some(filter) = matched_filter {
            if let Some(notify_path) = &filter.notify_path {
                if let Err(err) =
                    notify_observer_with_client(&state, notify_path, &test_id, &body_json, route)
                        .await
                {
                    warn!("failed to notify test container: {}", err);
                    verifier_notified = false;
                }
            } else {
                warn!(
                    "request matched test case {} but no notifyPath is configured",
                    test_id
                );
                verifier_notified = false;
            }
        } else {
            verifier_notified = false;
        }

        if verifier_notified
            && route.should_register_case()
            && let Err(err) = state.runtime.register_test_case(&test_id).await
        {
            warn!("failed to register case: {}", err);
        }
    }

    if matched
        && state.runtime.has_role(PluginRole::Mock)
        && should_mock_request(&state.config, matched_filter)
    {
        let test_id = state
            .config
            .test_id
            .as_ref()
            .and_then(|sel| extract_test_id(sel, &body_json, &path_and_query, &headers));
        return forward_mock_request(
            &state,
            matched_filter,
            test_id.as_deref(),
            route,
            &method,
            &headers,
            &body_bytes,
        )
        .await;
    }

    let Some(real_endpoint) = state.config.routed_proxy_target(route) else {
        return plain_response(
            axum::http::StatusCode::BAD_GATEWAY,
            "realEndpoint not configured",
        );
    };

    let real_url = join_target_url(&real_endpoint, &path_and_query);
    forward_raw_request(
        &state.client,
        &method,
        &real_url,
        &headers,
        BodyPayload::Buffered(body_bytes),
        Vec::new(),
    )
    .await
}

pub(crate) async fn write_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::Json(req): axum::Json<HttpWriteRequest>,
) -> impl IntoResponse {
    if let Err(err) = authorize_operator_response(&state, &headers) {
        return err;
    }
    if !state.runtime.has_role(PluginRole::Writer) {
        return (
            axum::http::StatusCode::NOT_FOUND,
            "fluidbg http writer role is not active".to_string(),
        );
    }

    let Some(_guard) =
        ActiveRequestGuard::try_new(state.mode.clone(), state.active_requests.clone())
    else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "fluidbg http plugin is not active".to_string(),
        );
    };
    let Some(target_url) = state.config.write_target() else {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            "targetUrl not configured".to_string(),
        );
    };

    let method = req.method.as_deref().unwrap_or("POST");
    let path = req.path.as_deref().unwrap_or("/");
    let url = join_target_url(&target_url, path);

    let mut builder = state
        .client
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

async fn notify_observer_with_client(
    state: &AppState,
    notify_path: &str,
    test_id: &str,
    payload: &Value,
    route: TrafficRoute,
) -> anyhow::Result<()> {
    let base = state
        .config
        .verifier_base_url(state.runtime.test_container_url());
    let path = render_path(notify_path, test_id, state.runtime.inception_point());
    let url = format!("{}{}", base.trim_end_matches('/'), path);
    let notification = ObservationNotification {
        test_id,
        inception_point: state.runtime.inception_point(),
        route: route.as_str(),
        payload,
    };
    let mut request = state.client.post(url).json(&notification);
    if let Some(auth_token) = state.runtime.auth_token() {
        request = request.header(AUTHORIZATION_HEADER, bearer_value(auth_token));
    }
    let response = request.send().await?;
    response.error_for_status()?;
    Ok(())
}

async fn forward_mock_request(
    state: &AppState,
    filter: Option<&fluidbg_plugin_sdk::NotificationFilter>,
    test_id: Option<&str>,
    route: TrafficRoute,
    method: &axum::http::Method,
    headers: &HeaderMap,
    body: &[u8],
) -> AxumResponse {
    let Some(mock_path) = filter
        .and_then(|filter| filter.mock_path.as_deref())
        .or(state.config.mock_path.as_deref())
    else {
        return plain_response(
            StatusCode::BAD_GATEWAY,
            "mockPath not configured for HTTP mock role",
        );
    };
    let test_id = test_id.unwrap_or("unknown");
    let base = state
        .config
        .verifier_base_url(state.runtime.test_container_url());
    let path = render_path(mock_path, test_id, state.runtime.inception_point());
    let url = format!("{}{}", base.trim_end_matches('/'), path);
    forward_raw_request(
        &state.client,
        method,
        &url,
        headers,
        BodyPayload::Buffered(Bytes::copy_from_slice(body)),
        verifier_headers(state, test_id, route),
    )
    .await
}

async fn forward_mock_streaming_request(
    state: &AppState,
    method: &axum::http::Method,
    path_and_query: &str,
    headers: &HeaderMap,
    body: Body,
) -> AxumResponse {
    let filter = matching_filter(
        &state.config,
        method.as_str(),
        path_and_query,
        headers,
        &Value::Null,
    );
    let Some(mock_path) = filter
        .and_then(|filter| filter.mock_path.as_deref())
        .or(state.config.mock_path.as_deref())
    else {
        return plain_response(
            StatusCode::BAD_GATEWAY,
            "mockPath not configured for HTTP mock role",
        );
    };
    let test_id = state
        .config
        .test_id
        .as_ref()
        .and_then(|sel| extract_test_id(sel, &Value::Null, path_and_query, headers))
        .unwrap_or_else(|| "unknown".to_string());
    let base = state
        .config
        .verifier_base_url(state.runtime.test_container_url());
    let path = render_path(mock_path, &test_id, state.runtime.inception_point());
    let url = format!("{}{}", base.trim_end_matches('/'), path);
    forward_streaming_request(
        &state.client,
        method,
        &url,
        headers,
        body,
        verifier_headers(state, &test_id, TrafficRoute::Blue),
    )
    .await
}

async fn forward_raw_request(
    client: &reqwest::Client,
    method: &axum::http::Method,
    url: &str,
    headers: &HeaderMap,
    body: BodyPayload,
    extra_headers: Vec<(String, String)>,
) -> AxumResponse {
    let mut req_builder = client.request(
        method.as_str().parse().unwrap_or(reqwest::Method::POST),
        url,
    );
    let overrides_authorization = extra_headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case(AUTHORIZATION_HEADER));

    for (name, value) in headers.iter() {
        if overrides_authorization && name.as_str().eq_ignore_ascii_case(AUTHORIZATION_HEADER) {
            continue;
        }
        if should_forward_request_header(name) {
            req_builder = req_builder.header(name, value);
        }
    }
    for (name, value) in extra_headers {
        req_builder = req_builder.header(name, value);
    }
    req_builder = match body {
        BodyPayload::Buffered(bytes) => req_builder.body(bytes),
        BodyPayload::Streaming(body) => {
            req_builder.body(reqwest::Body::wrap_stream(body.into_data_stream()))
        }
    };

    match req_builder.send().await {
        Ok(resp) => response_from_reqwest(resp).await,
        Err(e) => {
            warn!("upstream error for {}: {}", url, e);
            plain_response(axum::http::StatusCode::BAD_GATEWAY, "upstream error")
        }
    }
}

async fn forward_streaming_request(
    client: &reqwest::Client,
    method: &axum::http::Method,
    url: &str,
    headers: &HeaderMap,
    body: Body,
    extra_headers: Vec<(String, String)>,
) -> AxumResponse {
    forward_raw_request(
        client,
        method,
        url,
        headers,
        BodyPayload::Streaming(body),
        extra_headers,
    )
    .await
}

enum BodyPayload {
    Buffered(Bytes),
    Streaming(Body),
}

fn verifier_headers(state: &AppState, test_id: &str, route: TrafficRoute) -> Vec<(String, String)> {
    let mut headers = Vec::new();
    if let Some(auth_token) = state.runtime.auth_token() {
        headers.push((AUTHORIZATION_HEADER.to_string(), bearer_value(auth_token)));
    }
    headers.push(("x-fluidbg-test-id".to_string(), test_id.to_string()));
    headers.push((
        "x-fluidbg-inception-point".to_string(),
        state.runtime.inception_point().to_string(),
    ));
    headers.push((
        "x-fluidbg-blue-green-ref".to_string(),
        state.runtime.blue_green_ref().to_string(),
    ));
    headers.push(("x-fluidbg-route".to_string(), route.as_str().to_string()));
    headers
}

async fn response_from_reqwest(resp: reqwest::Response) -> AxumResponse {
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::OK);
    let headers = resp.headers().clone();
    let body = resp.bytes_stream();
    let mut builder = Response::builder().status(status);
    if let Some(target_headers) = builder.headers_mut() {
        for (name, value) in headers.iter() {
            if should_forward_response_header(name)
                && let (Ok(name), Ok(value)) = (
                    HeaderName::from_bytes(name.as_str().as_bytes()),
                    HeaderValue::from_bytes(value.as_bytes()),
                )
            {
                target_headers.append(name, value);
            }
        }
    }
    builder
        .body(Body::from_stream(body))
        .unwrap_or_else(|_| plain_response(StatusCode::INTERNAL_SERVER_ERROR, "response error"))
}

fn plain_response(status: StatusCode, body: &'static str) -> AxumResponse {
    (status, body).into_response()
}

fn should_forward_request_header(name: &HeaderName) -> bool {
    !is_hop_by_hop_header(name)
        && name != axum::http::header::HOST
        && name != axum::http::header::CONTENT_LENGTH
}

fn should_forward_response_header(name: &reqwest::header::HeaderName) -> bool {
    let Ok(name) = HeaderName::from_bytes(name.as_str().as_bytes()) else {
        return false;
    };
    !is_hop_by_hop_header(&name)
        && name != axum::http::header::CONTENT_LENGTH
        && name != axum::http::header::TRANSFER_ENCODING
}

fn is_hop_by_hop_header(name: &HeaderName) -> bool {
    matches!(
        name.as_str().to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

fn request_requires_buffer(state: &AppState) -> bool {
    state.runtime.has_role(PluginRole::Splitter)
        || state.runtime.has_role(PluginRole::Observer)
        || config_uses_body(&state.config)
}

fn matches_without_body(config: &Config, method: &str, path: &str, headers: &HeaderMap) -> bool {
    let root = matches_filter(&config.r#match, method, path, headers, &Value::Null);
    root && (!has_any_filters(config)
        || matching_filter(config, method, path, headers, &Value::Null).is_some())
}

fn join_target_url(base: &str, path_and_query: &str) -> String {
    let (path, query) = path_and_query
        .split_once('?')
        .map(|(path, query)| (path, Some(query)))
        .unwrap_or((path_and_query, None));
    let base = base.trim_end_matches('/');
    let joined = if path.is_empty() || path == "/" {
        base.to_string()
    } else {
        format!("{base}{path}")
    };
    match query {
        Some(query) if !query.is_empty() => format!("{joined}?{query}"),
        _ => joined,
    }
}

fn should_mock_request(
    config: &Config,
    filter: Option<&fluidbg_plugin_sdk::NotificationFilter>,
) -> bool {
    filter
        .and_then(|filter| filter.mock_path.as_deref())
        .or(config.mock_path.as_deref())
        .is_some()
}

fn config_uses_body(config: &Config) -> bool {
    selector_uses_body(config.test_id.as_ref())
        || conditions_use_body(&config.r#match)
        || config.filters.iter().any(filter_uses_body)
        || config
            .ingress
            .as_ref()
            .is_some_and(|direction| direction.filters.iter().any(filter_uses_body))
        || config
            .egress
            .as_ref()
            .is_some_and(|direction| direction.filters.iter().any(filter_uses_body))
}

fn selector_uses_body(selector: Option<&fluidbg_plugin_sdk::TestIdSelector>) -> bool {
    selector
        .and_then(|selector| selector.field.as_deref())
        .is_some_and(|field| field == "http.body")
}

fn filter_uses_body(filter: &fluidbg_plugin_sdk::NotificationFilter) -> bool {
    conditions_use_body(&filter.r#match)
}

fn conditions_use_body(conditions: &[fluidbg_plugin_sdk::FilterCondition]) -> bool {
    conditions
        .iter()
        .any(|condition| condition.field == "http.body")
}

pub(crate) async fn health() -> &'static str {
    "ok"
}

pub(crate) async fn prepare_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<axum::Json<PluginLifecycleResponse>, StatusCode> {
    authorize_operator(&state, &headers)?;
    state.set_runtime_mode(RuntimeMode::Idle);
    Ok(axum::Json(PluginLifecycleResponse::default()))
}

pub(crate) async fn activate_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<axum::Json<PluginLifecycleResponse>, StatusCode> {
    authorize_operator(&state, &headers)?;
    state.set_runtime_mode(RuntimeMode::Active);
    Ok(axum::Json(PluginLifecycleResponse::default()))
}

pub(crate) async fn drain_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<axum::Json<PluginLifecycleResponse>, StatusCode> {
    authorize_operator(&state, &headers)?;
    state.set_runtime_mode(RuntimeMode::Draining);
    Ok(axum::Json(PluginLifecycleResponse::default()))
}

pub(crate) async fn cleanup_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<axum::Json<PluginLifecycleResponse>, StatusCode> {
    authorize_operator(&state, &headers)?;
    state.set_runtime_mode(RuntimeMode::Draining);
    Ok(axum::Json(PluginLifecycleResponse::default()))
}

pub(crate) async fn drain_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<axum::Json<PluginDrainStatusResponse>, StatusCode> {
    authorize_operator(&state, &headers)?;
    let active = state.active_requests.load(Ordering::SeqCst);
    let mode = state.runtime_mode();
    let drained = mode == RuntimeMode::Draining && active == 0;
    let message = if drained {
        "http plugin is draining and has no admitted proxy/write requests".to_string()
    } else if mode == RuntimeMode::Idle {
        "http plugin is idle and has not been activated".to_string()
    } else if mode == RuntimeMode::Active {
        "http plugin is active and has not entered drain mode".to_string()
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
    if !state.runtime.has_role(PluginRole::Splitter) {
        return Err(StatusCode::BAD_REQUEST);
    }
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

fn authorize_operator_response(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(), (StatusCode, String)> {
    let header = headers
        .get(AUTHORIZATION_HEADER)
        .and_then(|value| value.to_str().ok());
    require_bearer_token(header, state.runtime.auth_token())
        .map_err(|_| (StatusCode::UNAUTHORIZED, "unauthorized".to_string()))
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderName};
    use fluidbg_plugin_sdk::{FilterCondition, NotificationFilter, TestIdSelector};

    use crate::config::{ClientTlsConfig, Config, TlsConfig};

    use super::{
        config_uses_body, join_target_url, matches_without_body, should_forward_response_header,
        should_mock_request,
    };

    fn config() -> Config {
        Config {
            port: None,
            proxy_protocol: None,
            write_protocol: None,
            real_endpoint: Some("http://upstream".to_string()),
            target_url: None,
            green_endpoint: None,
            blue_endpoint: None,
            env_var_name: None,
            write_env_var: None,
            verifier_endpoint: None,
            mock_path: Some("/mock/{testId}".to_string()),
            test_id: Some(TestIdSelector {
                field: Some("http.path".to_string()),
                json_path: None,
                path_segment: Some(1),
                value: None,
            }),
            r#match: vec![FilterCondition {
                field: "http.query.kind".to_string(),
                equals: Some("mock".to_string()),
                matches: None,
                json_path: None,
            }],
            filters: vec![NotificationFilter {
                r#match: vec![FilterCondition {
                    field: "http.path".to_string(),
                    equals: Some("/orders/17".to_string()),
                    matches: None,
                    json_path: None,
                }],
                notify_path: None,
                mock_path: Some("/mock/{testId}/specific".to_string()),
                payload: None,
            }],
            ingress: None,
            egress: None,
            tls: TlsConfig::default(),
            client_tls: ClientTlsConfig::default(),
        }
    }

    #[test]
    fn non_body_mock_config_can_stream_request_body() {
        assert!(!config_uses_body(&config()));
    }

    #[test]
    fn body_filter_requires_bounded_buffer() {
        let mut config = config();
        config.filters[0].r#match.push(FilterCondition {
            field: "http.body".to_string(),
            equals: None,
            matches: Some("order".to_string()),
            json_path: Some("$.type".to_string()),
        });
        assert!(config_uses_body(&config));
    }

    #[test]
    fn path_and_query_filters_work_without_body() {
        assert!(matches_without_body(
            &config(),
            "POST",
            "/orders/17?kind=mock",
            &HeaderMap::new()
        ));
    }

    #[test]
    fn mock_role_does_not_mock_observer_only_filter() {
        let mut config = config();
        config.mock_path = None;
        config.filters[0].mock_path = None;
        assert!(!should_mock_request(&config, Some(&config.filters[0])));

        config.filters[0].mock_path = Some("/mock/{testId}".to_string());
        assert!(should_mock_request(&config, Some(&config.filters[0])));
    }

    #[test]
    fn target_url_join_preserves_configured_endpoint_for_root_proxy_requests() {
        assert_eq!(
            join_target_url("http://httpbin.fluidbg-system/post", "/?via=fluidbg"),
            "http://httpbin.fluidbg-system/post?via=fluidbg"
        );
        assert_eq!(
            join_target_url("http://upstream/api", "/orders?via=fluidbg"),
            "http://upstream/api/orders?via=fluidbg"
        );
    }

    #[test]
    fn response_header_filter_drops_hop_by_hop_headers_only() {
        assert!(should_forward_response_header(
            &reqwest::header::CONTENT_TYPE
        ));
        assert!(!should_forward_response_header(&HeaderName::from_static(
            "connection"
        )));
    }
}
