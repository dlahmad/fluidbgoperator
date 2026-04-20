use anyhow::Result;
use lapin::options::{BasicPublishOptions, QueueDeclareOptions};
use lapin::types::FieldTable;
use lapin::{BasicProperties, Channel, Connection, ConnectionProperties};
use serde::Deserialize;
use tracing::info;

#[derive(Debug, Deserialize)]
struct Config {
    target_queue: String,
    #[serde(default)]
    amqp_url: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
#[allow(dead_code)]
struct WriteRequest {
    test_id: Option<String>,
    payload: serde_json::Value,
    properties: Option<serde_json::Value>,
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
    let amqp_url = config
        .amqp_url
        .as_deref()
        .unwrap_or("amqp://fluidbg:fluidbg@rabbitmq.fluidbg-system:5672/");

    info!("rabbitmq-writer starting: target={}", config.target_queue);

    let conn = Connection::connect(amqp_url, ConnectionProperties::default()).await?;
    let channel = conn.create_channel().await?;

    channel
        .queue_declare(
            &config.target_queue,
            QueueDeclareOptions::default(),
            FieldTable::default(),
        )
        .await?;

    let app = axum::Router::new()
        .route("/health", axum::routing::get(health))
        .route("/write", axum::routing::post(write_handler))
        .with_state((channel, config.target_queue.clone()));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:9090").await?;
    info!("rabbitmq-writer listening on 0.0.0.0:9090");
    axum::serve(listener, app).await?;

    Ok(())
}

async fn health() -> &'static str {
    "ok"
}

async fn write_handler(
    axum::extract::State((channel, queue)): axum::extract::State<(Channel, String)>,
    axum::Json(req): axum::Json<WriteRequest>,
) -> impl axum::response::IntoResponse {
    let body = serde_json::to_vec(&req.payload).unwrap_or_default();

    let mut props = BasicProperties::default();
    if let Some(props_value) = &req.properties
        && let Some(headers) = props_value.get("headers")
        && let Ok(h) = serde_json::from_value::<FieldTable>(headers.clone())
    {
        props = props.with_headers(h);
    }

    match channel
        .basic_publish("", &queue, BasicPublishOptions::default(), &body, props)
        .await
    {
        Ok(_) => (axum::http::StatusCode::OK, "published"),
        Err(e) => {
            tracing::error!("failed to publish: {}", e);
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "publish failed",
            )
        }
    }
}
