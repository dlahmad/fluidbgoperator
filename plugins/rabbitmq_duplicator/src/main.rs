use anyhow::Result;
use chrono::Utc;
use futures_lite::StreamExt;
use lapin::options::{
    BasicAckOptions, BasicConsumeOptions, BasicPublishOptions, QueueDeclareOptions,
};
use lapin::types::{AMQPValue, FieldTable};
use lapin::{BasicProperties, Connection, ConnectionProperties};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{info, warn};

#[derive(Debug, Deserialize)]
struct Config {
    source_queue: String,
    shadow_queue: String,
    #[serde(default)]
    amqp_url: Option<String>,
    #[serde(default)]
    test_id: Option<TestIdSelector>,
    #[serde(default)]
    r#match: Vec<FilterCondition>,
    #[serde(default)]
    notify_path: Option<String>,
    #[serde(default)]
    traffic_percent: Option<i32>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct TestIdSelector {
    field: Option<String>,
    json_path: Option<String>,
    value: Option<String>,
    path_segment: Option<i32>,
}

#[derive(Debug, Deserialize)]
struct FilterCondition {
    field: String,
    #[serde(default)]
    equals: Option<String>,
    #[serde(default)]
    matches: Option<String>,
    #[serde(default)]
    json_path: Option<String>,
}

#[derive(Debug, Serialize)]
struct CaseRegistration {
    test_id: String,
    blue_green_ref: String,
    inception_point: String,
    triggered_at: String,
    timeout_seconds: i64,
}

#[derive(Debug, Serialize)]
struct TriggerNotification {
    test_id: String,
    inception_point: String,
    payload: Value,
    timestamp: String,
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

fn amqp_value_as_string(v: &AMQPValue) -> Option<String> {
    match v {
        AMQPValue::LongString(s) => Some(s.to_string()),
        AMQPValue::ShortString(s) => Some(s.as_str().to_string()),
        _ => None,
    }
}

fn extract_test_id(
    selector: &TestIdSelector,
    body: &Value,
    properties: &FieldTable,
) -> Option<String> {
    if let Some(val) = &selector.value {
        return Some(val.clone());
    }
    let field = selector.field.as_ref()?;
    match field.as_str() {
        "queue.body" => {
            if let Some(jp) = &selector.json_path {
                extract_json_path(body, jp)
            } else {
                body.as_str().map(|s| s.to_string())
            }
        }
        f if f.starts_with("queue.property.") => {
            let key = f.strip_prefix("queue.property.")?;
            properties.inner().get(key).and_then(amqp_value_as_string)
        }
        _ => None,
    }
}

fn matches_filter(conditions: &[FilterCondition], body: &Value, properties: &FieldTable) -> bool {
    if conditions.is_empty() {
        return true;
    }
    conditions.iter().all(|c| {
        let value = match c.field.as_str() {
            "queue.body" => {
                if let Some(jp) = &c.json_path {
                    extract_json_path(body, jp)
                } else {
                    body.as_str().map(|s| s.to_string())
                }
            }
            f if f.starts_with("queue.property.") => {
                let key = f.strip_prefix("queue.property.").unwrap_or("");
                properties.inner().get(key).and_then(amqp_value_as_string)
            }
            _ => None,
        };
        let value = match value {
            Some(v) => v,
            None => return false,
        };
        if let Some(eq) = &c.equals {
            &value == eq
        } else if let Some(re) = &c.matches {
            Regex::new(re).map(|r| r.is_match(&value)).unwrap_or(false)
        } else {
            true
        }
    })
}

async fn register_case(
    client: &reqwest::Client,
    operator_url: &str,
    test_id: &str,
    inception_point: &str,
    timeout_seconds: i64,
) -> Result<()> {
    let reg = CaseRegistration {
        test_id: test_id.to_string(),
        blue_green_ref: String::new(),
        inception_point: inception_point.to_string(),
        triggered_at: Utc::now().to_rfc3339(),
        timeout_seconds,
    };
    client
        .post(format!("{}/cases", operator_url.trim_end_matches('/')))
        .json(&reg)
        .send()
        .await?;
    Ok(())
}

async fn notify_test_container(
    client: &reqwest::Client,
    test_container_url: &str,
    notify_path: &str,
    test_id: &str,
    inception_point: &str,
    payload: &Value,
) -> Result<()> {
    let path = notify_path
        .replace("{testId}", test_id)
        .replace("{inceptionPoint}", inception_point);
    let notif = TriggerNotification {
        test_id: test_id.to_string(),
        inception_point: inception_point.to_string(),
        payload: payload.clone(),
        timestamp: Utc::now().to_rfc3339(),
    };
    client
        .post(format!(
            "{}{}",
            test_container_url.trim_end_matches('/'),
            path
        ))
        .json(&notif)
        .send()
        .await?;
    Ok(())
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
    let operator_url = std::env::var("FLUIDBG_OPERATOR_URL")
        .unwrap_or_else(|_| "http://localhost:8090".to_string());
    let test_container_url = std::env::var("FLUIDBG_TEST_CONTAINER_URL")
        .unwrap_or_else(|_| "http://localhost:8080".to_string());
    let inception_point =
        std::env::var("FLUIDBG_INCEPTION_POINT").unwrap_or_else(|_| "unknown".to_string());
    let mode = std::env::var("FLUIDBG_MODE").unwrap_or_else(|_| "trigger".to_string());
    let timeout_seconds: i64 = std::env::var("FLUIDBG_TIMEOUT_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);

    info!(
        "rabbitmq-duplicator starting: source={}, shadow={}, mode={}",
        config.source_queue, config.shadow_queue, mode
    );

    let conn = Connection::connect(amqp_url, ConnectionProperties::default()).await?;
    let channel = conn.create_channel().await?;

    channel
        .queue_declare(
            &config.source_queue,
            QueueDeclareOptions::default(),
            FieldTable::default(),
        )
        .await?;

    channel
        .queue_declare(
            &config.shadow_queue,
            QueueDeclareOptions::default(),
            FieldTable::default(),
        )
        .await?;

    let mut consumer = channel
        .basic_consume(
            &config.source_queue,
            "fluidbg-duplicator",
            BasicConsumeOptions::default(),
            FieldTable::default(),
        )
        .await?;

    let http_client = reqwest::Client::new();
    let traffic_percent = config.traffic_percent.unwrap_or(100);

    while let Some(delivery) = consumer.next().await {
        let delivery = delivery?;
        let body_data = delivery.data.clone();
        let body_str = String::from_utf8_lossy(&body_data);
        let body_json: Value = serde_json::from_str(&body_str).unwrap_or(Value::Null);

        let properties = delivery.properties.clone();
        let headers = properties.headers().clone().unwrap_or_default();

        if !matches_filter(&config.r#match, &body_json, &headers) {
            delivery.ack(BasicAckOptions::default()).await?;
            continue;
        }

        let test_id = config
            .test_id
            .as_ref()
            .and_then(|sel| extract_test_id(sel, &body_json, &headers));

        let should_duplicate =
            traffic_percent >= 100 || (rand::random::<f32>() * 100.0) < traffic_percent as f32;

        if should_duplicate {
            channel
                .basic_publish(
                    "",
                    &config.shadow_queue,
                    BasicPublishOptions::default(),
                    &body_data,
                    BasicProperties::default().with_headers(headers),
                )
                .await?;
        }

        if mode == "trigger" {
            if let Some(tid) = &test_id {
                if let Err(e) = register_case(
                    &http_client,
                    &operator_url,
                    tid,
                    &inception_point,
                    timeout_seconds,
                )
                .await
                {
                    warn!("failed to register case {}: {}", tid, e);
                }

                if let Some(notify_path) = &config.notify_path
                    && let Err(e) = notify_test_container(
                        &http_client,
                        &test_container_url,
                        notify_path,
                        tid,
                        &inception_point,
                        &body_json,
                    )
                    .await
                {
                    warn!("failed to notify test container for {}: {}", tid, e);
                }
            } else {
                warn!("could not extract testId from message");
            }
        }

        delivery.ack(BasicAckOptions::default()).await?;
    }

    Ok(())
}
