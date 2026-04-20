use anyhow::Result;
use serde::Deserialize;
use tracing::info;

#[derive(Debug, Deserialize)]
struct Config {
    target_url: String,
    #[serde(default)]
    #[allow(dead_code)]
    write_env_var: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct WriteRequest {
    #[allow(dead_code)]
    test_id: Option<String>,
    payload: serde_json::Value,
    method: Option<String>,
    headers: Option<serde_json::Value>,
    path: Option<String>,
}

fn load_config() -> Result<Config> {
    let path = std::env::var("FLUIDBG_CONFIG_PATH")
        .unwrap_or_else(|_| "/etc/fluidbg/config.yaml".to_string());
    let data = std::fs::read_to_string(&path)?;
    Ok(serde_yaml_ng::from_str(&data)?)
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

    info!("http-writer starting: target={}", config.target_url);

    let app = axum::Router::new()
        .route("/health", axum::routing::get(health))
        .route("/write", axum::routing::post(write_handler))
        .with_state(config.target_url.clone());

    let listener = tokio::net::TcpListener::bind("0.0.0.0:9090").await?;
    info!("http-writer listening on 0.0.0.0:9090");
    axum::serve(listener, app).await?;

    Ok(())
}

async fn health() -> &'static str {
    "ok"
}

async fn write_handler(
    axum::extract::State(target_url): axum::extract::State<String>,
    axum::Json(req): axum::Json<WriteRequest>,
) -> impl axum::response::IntoResponse {
    let method = req.method.as_deref().unwrap_or("POST");
    let path = req.path.as_deref().unwrap_or("/");
    let url = format!("{}{}", target_url.trim_end_matches('/'), path);

    let client = reqwest::Client::new();
    let mut builder = client.request(method.parse().unwrap_or(reqwest::Method::POST), &url);

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
