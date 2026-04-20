use anyhow::Result;
use chrono::Utc;
use regex::Regex;
use serde::Deserialize;
use serde_json::Value;
use tracing::{info, warn};

#[derive(Debug, Deserialize, Clone)]
struct Config {
    proxy_port: u16,
    real_endpoint: String,
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
    filters: Vec<FilterDef>,
    #[serde(default)]
    #[allow(dead_code)]
    ingress: Option<DirectionConfig>,
    #[serde(default)]
    #[allow(dead_code)]
    egress: Option<DirectionConfig>,
}

#[derive(Debug, Deserialize, Clone)]
struct TestIdSelector {
    field: Option<String>,
    json_path: Option<String>,
    value: Option<String>,
    path_segment: Option<i32>,
}

#[derive(Debug, Deserialize, Clone)]
struct FilterCondition {
    field: String,
    equals: Option<String>,
    matches: Option<String>,
    json_path: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
struct FilterDef {
    #[allow(dead_code)]
    r#match: Vec<FilterCondition>,
    notify_path: Option<String>,
    #[allow(dead_code)]
    payload: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
struct DirectionConfig {
    #[serde(default)]
    #[allow(dead_code)]
    filters: Vec<FilterDef>,
}

fn load_config() -> Result<Config> {
    let path = std::env::var("FLUIDBG_CONFIG_PATH")
        .unwrap_or_else(|_| "/etc/fluidbg/config.yaml".to_string());
    let data = std::fs::read_to_string(&path)?;
    Ok(serde_yaml_ng::from_str(&data)?)
}

fn extract_json_path(value: &Value, path: &str) -> Option<String> {
    let stripped = path.strip_prefix('$').unwrap_or(path);
    let stripped = stripped.strip_prefix('.').unwrap_or(stripped);
    let keys: Vec<&str> = stripped.split('.').collect();
    let mut current = value;
    for key in &keys {
        current = current.as_object()?.get(*key)?;
    }
    match current {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn matches_filter(
    conditions: &[FilterCondition],
    method: &str,
    path: &str,
    headers: &axum::http::HeaderMap,
    body: &Value,
) -> bool {
    if conditions.is_empty() {
        return true;
    }
    conditions.iter().all(|c| {
        let value = resolve_field(c, method, path, headers, body);
        let Some(value) = value else {
            return false;
        };
        if let Some(eq) = &c.equals {
            value == *eq
        } else if let Some(re) = &c.matches {
            Regex::new(re).map(|r| r.is_match(&value)).unwrap_or(false)
        } else {
            true
        }
    })
}

fn resolve_field(
    c: &FilterCondition,
    method: &str,
    path: &str,
    headers: &axum::http::HeaderMap,
    body: &Value,
) -> Option<String> {
    match c.field.as_str() {
        "http.method" => Some(method.to_string()),
        "http.path" => Some(path.to_string()),
        "http.body" => {
            if let Some(jp) = &c.json_path {
                extract_json_path(body, jp)
            } else {
                body.as_str().map(|s| s.to_string())
            }
        }
        f if f.starts_with("http.header.") => {
            let key = f.strip_prefix("http.header.")?;
            headers
                .get(key)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
        }
        f if f.starts_with("http.query.") => {
            let key = f.strip_prefix("http.query.")?;
            let query_str = path.split('?').nth(1).unwrap_or("");
            for pair in query_str.split('&') {
                let mut kv = pair.splitn(2, '=');
                if kv.next() == Some(key)
                    && let Some(v) = kv.next()
                {
                    return Some(v.to_string());
                }
            }
            None
        }
        _ => None,
    }
}

fn extract_test_id(
    sel: &TestIdSelector,
    body: &Value,
    path: &str,
    headers: &axum::http::HeaderMap,
) -> Option<String> {
    if let Some(val) = &sel.value {
        return Some(val.clone());
    }
    let field = sel.field.as_ref()?;
    match field.as_str() {
        "http.body" => {
            if let Some(jp) = &sel.json_path {
                extract_json_path(body, jp)
            } else {
                body.as_str().map(|s| s.to_string())
            }
        }
        "http.path" => {
            if let Some(seg) = sel.path_segment {
                path.split('/')
                    .filter(|s| !s.is_empty())
                    .nth((seg - 1) as usize)
                    .map(|s| s.to_string())
            } else {
                Some(path.to_string())
            }
        }
        f if f.starts_with("http.header.") => {
            let key = f.strip_prefix("http.header.")?;
            headers
                .get(key)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
        }
        _ => None,
    }
}

async fn register_case(operator_url: &str, test_id: &str, inception_point: &str) {
    let client = reqwest::Client::new();
    if let Err(e) = client
        .post(format!("{}/cases", operator_url.trim_end_matches('/')))
        .json(&serde_json::json!({
            "testId": test_id,
            "blueGreenRef": "",
            "inceptionPoint": inception_point,
            "triggeredAt": Utc::now().to_rfc3339(),
            "timeoutSeconds": 60
        }))
        .send()
        .await
    {
        warn!("failed to register case: {}", e);
    }
}

async fn notify_observation(
    test_container_url: &str,
    notify_path: &str,
    inception_point: &str,
    body: &Value,
    method: &str,
    path: &str,
) {
    let client = reqwest::Client::new();
    let url = format!(
        "{}{}",
        test_container_url.trim_end_matches('/'),
        notify_path
    );
    if let Err(e) = client
        .post(&url)
        .json(&serde_json::json!({
            "inceptionPoint": inception_point,
            "direction": "egress",
            "mode": "passthrough-duplicate",
            "request": {
                "method": method,
                "path": path,
                "body": body
            },
            "timestamp": Utc::now().to_rfc3339()
        }))
        .send()
        .await
    {
        warn!("failed to notify test container: {}", e);
    }
}

async fn proxy_handler(
    axum::extract::State(state): axum::extract::State<AppState>,
    req: axum::extract::Request,
) -> impl axum::response::IntoResponse {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let path = uri.path().to_string();
    let headers = req.headers().clone();

    let body_bytes = axum::body::to_bytes(req.into_body(), 10 * 1024 * 1024)
        .await
        .unwrap_or_default();
    let body_json: Value = serde_json::from_slice(&body_bytes).unwrap_or(Value::Null);

    let matched = matches_filter(
        &state.config.r#match,
        method.as_str(),
        &path,
        &headers,
        &body_json,
    );

    if matched {
        if state.mode == "trigger" || state.mode == "passthrough-duplicate" {
            if let Some(filter) = state.config.filters.first()
                && let Some(notify_path) = &filter.notify_path
            {
                notify_observation(
                    &state.test_container_url,
                    notify_path,
                    &state.inception_point,
                    &body_json,
                    method.as_str(),
                    &path,
                )
                .await;
            }

            if state.mode == "trigger"
                && let Some(sel) = &state.config.test_id
                && let Some(test_id) = extract_test_id(sel, &body_json, &path, &headers)
            {
                register_case(&state.operator_url, &test_id, &state.inception_point).await;
            }
        }

        if state.mode == "reroute-mock" {
            return (axum::http::StatusCode::OK, "mocked by fluidbg".to_string());
        }
    }

    let client = reqwest::Client::new();
    let real_url = format!(
        "{}{}",
        state.config.real_endpoint.trim_end_matches('/'),
        path
    );
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

#[derive(Clone)]
struct AppState {
    config: Config,
    mode: String,
    operator_url: String,
    test_container_url: String,
    inception_point: String,
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
    let mode =
        std::env::var("FLUIDBG_MODE").unwrap_or_else(|_| "passthrough-duplicate".to_string());
    let operator_url = std::env::var("FLUIDBG_OPERATOR_URL")
        .unwrap_or_else(|_| "http://localhost:8090".to_string());
    let test_container_url = std::env::var("FLUIDBG_TEST_CONTAINER_URL")
        .unwrap_or_else(|_| "http://localhost:8080".to_string());
    let inception_point =
        std::env::var("FLUIDBG_INCEPTION_POINT").unwrap_or_else(|_| "unknown".to_string());

    info!(
        "http-proxy starting on port {}: mode={}, real={}",
        config.proxy_port, mode, config.real_endpoint
    );

    let state = AppState {
        config,
        mode,
        operator_url,
        test_container_url,
        inception_point,
    };

    let port = state.config.proxy_port;

    let app = axum::Router::new()
        .fallback(proxy_handler)
        .with_state(state);

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("http-proxy listening on {}", addr);
    axum::serve(listener, app).await?;

    Ok(())
}
