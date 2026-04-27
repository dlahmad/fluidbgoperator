use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use anyhow::Result;
use fluidbg_plugin_sdk::{
    FilterCondition, HttpWriteRequest, NotificationFilter, PluginDrainStatusResponse,
    PluginLifecycleResponse, PluginRole, PluginRuntime, TestIdSelector, TrafficRoute,
    TrafficShiftRequest, TrafficShiftResponse, extract_http_test_id, match_conditions,
    resolve_http_field, routes_to_blue, traffic_percent_from_env,
};
use serde::Deserialize;
use serde_json::Value;
use tracing::{info, warn};

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Config {
    #[serde(default)]
    #[allow(dead_code)]
    proxy_port: Option<u16>,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    real_endpoint: Option<String>,
    #[serde(default)]
    target_url: Option<String>,
    #[serde(default)]
    green_endpoint: Option<String>,
    #[serde(default)]
    blue_endpoint: Option<String>,
    #[serde(default)]
    env_var_name: Option<String>,
    #[serde(default)]
    write_env_var: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    ingress_port: Option<u16>,
    #[serde(default)]
    #[allow(dead_code)]
    egress_port: Option<u16>,
    #[serde(default)]
    test_id: Option<TestIdSelector>,
    #[serde(default)]
    r#match: Vec<FilterCondition>,
    #[serde(default)]
    filters: Vec<NotificationFilter>,
    #[serde(default)]
    ingress: Option<DirectionConfig>,
    #[serde(default)]
    egress: Option<DirectionConfig>,
}

impl Config {
    fn listen_port(&self) -> u16 {
        self.port.unwrap_or(9090)
    }

    fn routed_proxy_target(&self, route: TrafficRoute) -> Option<&str> {
        match route {
            TrafficRoute::Blue => self
                .blue_endpoint
                .as_deref()
                .or(self.real_endpoint.as_deref()),
            TrafficRoute::Green => self
                .green_endpoint
                .as_deref()
                .or(self.real_endpoint.as_deref()),
            _ => self.real_endpoint.as_deref(),
        }
    }

    fn write_target(&self) -> Option<&str> {
        self.target_url.as_deref().or(self.real_endpoint.as_deref())
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct DirectionConfig {
    #[serde(default)]
    filters: Vec<NotificationFilter>,
}

#[derive(Clone)]
struct AppState {
    config: Config,
    runtime: PluginRuntime,
    draining: Arc<AtomicBool>,
    active_requests: Arc<AtomicUsize>,
    traffic_percent: Arc<AtomicUsize>,
}

struct ActiveRequestGuard {
    active_requests: Arc<AtomicUsize>,
}

impl ActiveRequestGuard {
    fn new(active_requests: Arc<AtomicUsize>) -> Self {
        active_requests.fetch_add(1, Ordering::SeqCst);
        Self { active_requests }
    }
}

impl Drop for ActiveRequestGuard {
    fn drop(&mut self) {
        self.active_requests.fetch_sub(1, Ordering::SeqCst);
    }
}

fn load_config() -> Result<Config> {
    fluidbg_plugin_sdk::load_yaml_config()
}

fn matches_filter(
    conditions: &[FilterCondition],
    method: &str,
    path: &str,
    headers: &axum::http::HeaderMap,
    body: &Value,
) -> bool {
    match_conditions(conditions, |condition| {
        resolve_field(condition, method, path, headers, body)
    })
}

fn resolve_field(
    c: &FilterCondition,
    method: &str,
    path: &str,
    headers: &axum::http::HeaderMap,
    body: &Value,
) -> Option<String> {
    resolve_http_field(c, method, path, body, |key| {
        headers
            .get(key)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.to_string())
    })
}

fn matching_filter<'a>(
    config: &'a Config,
    method: &str,
    path: &str,
    headers: &axum::http::HeaderMap,
    body: &Value,
) -> Option<&'a NotificationFilter> {
    let direction_filters = config
        .ingress
        .as_ref()
        .into_iter()
        .chain(config.egress.as_ref())
        .flat_map(|direction| direction.filters.iter());
    config
        .filters
        .iter()
        .chain(direction_filters)
        .find(|filter| matches_filter(&filter.r#match, method, path, headers, body))
}

fn has_any_filters(config: &Config) -> bool {
    !config.filters.is_empty()
        || config
            .ingress
            .as_ref()
            .is_some_and(|direction| !direction.filters.is_empty())
        || config
            .egress
            .as_ref()
            .is_some_and(|direction| !direction.filters.is_empty())
}

fn extract_test_id(
    sel: &TestIdSelector,
    body: &Value,
    path: &str,
    headers: &axum::http::HeaderMap,
) -> Option<String> {
    extract_http_test_id(sel, body, path, |key| {
        headers
            .get(key)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.to_string())
    })
}

async fn proxy_handler(
    axum::extract::State(state): axum::extract::State<AppState>,
    req: axum::extract::Request,
) -> impl axum::response::IntoResponse {
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

async fn write_handler(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::Json(req): axum::Json<HttpWriteRequest>,
) -> impl axum::response::IntoResponse {
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

async fn health() -> &'static str {
    "ok"
}

async fn prepare_handler(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> axum::Json<PluginLifecycleResponse> {
    state.draining.store(false, Ordering::SeqCst);
    axum::Json(PluginLifecycleResponse::default())
}

async fn drain_handler(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> axum::Json<PluginLifecycleResponse> {
    state.draining.store(true, Ordering::SeqCst);
    axum::Json(PluginLifecycleResponse::default())
}

async fn cleanup_handler(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> axum::Json<PluginLifecycleResponse> {
    state.draining.store(true, Ordering::SeqCst);
    axum::Json(PluginLifecycleResponse::default())
}

async fn drain_status(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> axum::Json<PluginDrainStatusResponse> {
    let active = state.active_requests.load(Ordering::SeqCst);
    let drained = active == 0;
    let message = if drained {
        "http plugin has no active proxy/write requests".to_string()
    } else {
        format!("http plugin still has {active} active proxy/write request(s)")
    };
    axum::Json(PluginDrainStatusResponse {
        drained,
        message: Some(message),
    })
}

async fn traffic_shift_handler(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::Json(req): axum::Json<TrafficShiftRequest>,
) -> axum::Json<TrafficShiftResponse> {
    state
        .traffic_percent
        .store(req.traffic_percent.min(100) as usize, Ordering::SeqCst);
    axum::Json(TrafficShiftResponse {
        traffic_percent: state.traffic_percent.load(Ordering::SeqCst) as u8,
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config = load_config()?;
    let runtime = PluginRuntime::from_env();
    let port = config.listen_port();

    info!(
        "http plugin starting on port {}: roles={:?}, mode={}, real={:?}, target={:?}, envVar={:?}, writeEnvVar={:?}",
        port,
        runtime.roles(),
        runtime.mode(),
        config.real_endpoint,
        config.target_url,
        config.env_var_name,
        config.write_env_var
    );

    let state = AppState {
        config,
        runtime,
        draining: Arc::new(AtomicBool::new(false)),
        active_requests: Arc::new(AtomicUsize::new(0)),
        traffic_percent: Arc::new(AtomicUsize::new(traffic_percent_from_env() as usize)),
    };

    let app = axum::Router::new()
        .route("/health", axum::routing::get(health))
        .route("/prepare", axum::routing::post(prepare_handler))
        .route("/drain", axum::routing::post(drain_handler))
        .route("/cleanup", axum::routing::post(cleanup_handler))
        .route("/drain-status", axum::routing::get(drain_status))
        .route("/traffic", axum::routing::post(traffic_shift_handler))
        .route("/write", axum::routing::post(write_handler))
        .fallback(proxy_handler)
        .with_state(state);

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("http plugin listening on {}", addr);
    axum::serve(listener, app).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderValue};

    #[test]
    fn extracts_test_id_from_json_body_without_route_content() {
        let selector = TestIdSelector {
            field: Some("http.body".to_string()),
            json_path: Some("$.orderId".to_string()),
            path_segment: None,
            value: None,
        };
        let body = serde_json::json!({
            "orderId": "order-17",
            "status": "created"
        });
        let headers = HeaderMap::new();

        assert_eq!(
            extract_test_id(&selector, &body, "/orders", &headers),
            Some("order-17".to_string())
        );
    }

    #[test]
    fn matching_filter_uses_filter_specific_conditions() {
        let config = Config {
            proxy_port: None,
            port: None,
            real_endpoint: Some("http://upstream".to_string()),
            target_url: None,
            green_endpoint: None,
            blue_endpoint: None,
            env_var_name: None,
            write_env_var: None,
            ingress_port: None,
            egress_port: None,
            test_id: None,
            r#match: Vec::new(),
            filters: vec![
                NotificationFilter {
                    r#match: vec![FilterCondition {
                        field: "http.path".to_string(),
                        equals: Some("/ignored".to_string()),
                        matches: None,
                        json_path: None,
                    }],
                    notify_path: Some("/ignored/{testId}".to_string()),
                    payload: None,
                },
                NotificationFilter {
                    r#match: vec![FilterCondition {
                        field: "http.header.X-Event".to_string(),
                        equals: Some("order-created".to_string()),
                        matches: None,
                        json_path: None,
                    }],
                    notify_path: Some("/observe/{testId}/orders".to_string()),
                    payload: None,
                },
            ],
            ingress: None,
            egress: None,
        };
        let mut headers = HeaderMap::new();
        headers.insert("X-Event", HeaderValue::from_static("order-created"));

        let filter = matching_filter(
            &config,
            "POST",
            "/orders",
            &headers,
            &serde_json::json!({"orderId": "order-17"}),
        )
        .expect("expected second filter to match");

        assert_eq!(
            filter.notify_path.as_deref(),
            Some("/observe/{testId}/orders")
        );
    }

    #[test]
    fn route_metadata_is_plugin_supplied_not_payload_supplied() {
        let notification = fluidbg_plugin_sdk::ObservationNotification {
            test_id: "order-17",
            inception_point: "incoming-orders",
            route: TrafficRoute::Blue.as_str(),
            payload: &serde_json::json!({"orderId": "order-17"}),
        };

        let encoded = serde_json::to_value(notification).unwrap();

        assert_eq!(encoded["route"], "blue");
        assert!(encoded["payload"].get("route").is_none());
    }

    #[test]
    fn write_target_defaults_to_proxy_target() {
        let config = Config {
            proxy_port: None,
            port: None,
            real_endpoint: Some("http://blue".to_string()),
            target_url: None,
            green_endpoint: None,
            blue_endpoint: None,
            env_var_name: None,
            write_env_var: None,
            ingress_port: None,
            egress_port: None,
            test_id: None,
            r#match: Vec::new(),
            filters: Vec::new(),
            ingress: None,
            egress: None,
        };

        assert_eq!(config.write_target(), Some("http://blue"));
    }

    #[test]
    fn splitter_route_selects_specific_target() {
        let config = Config {
            proxy_port: None,
            port: None,
            real_endpoint: Some("http://fallback".to_string()),
            target_url: None,
            green_endpoint: Some("http://green".to_string()),
            blue_endpoint: Some("http://blue".to_string()),
            env_var_name: None,
            write_env_var: None,
            ingress_port: None,
            egress_port: None,
            test_id: None,
            r#match: Vec::new(),
            filters: Vec::new(),
            ingress: None,
            egress: None,
        };

        assert_eq!(
            config.routed_proxy_target(TrafficRoute::Green),
            Some("http://green")
        );
        assert_eq!(
            config.routed_proxy_target(TrafficRoute::Blue),
            Some("http://blue")
        );
    }
}
