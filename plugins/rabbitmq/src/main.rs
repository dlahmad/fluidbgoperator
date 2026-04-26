use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use axum::{Json, Router, extract::State, routing::{get, post}};
use futures_lite::StreamExt;
use lapin::options::{
    BasicAckOptions, BasicConsumeOptions, BasicGetOptions, BasicPublishOptions, QueueDeclareOptions,
    QueueDeleteOptions,
};
use lapin::types::{AMQPValue, FieldTable};
use lapin::{BasicProperties, Channel, Connection, ConnectionProperties};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{info, warn};

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Config {
    #[serde(default)]
    amqp_url: Option<String>,
    #[serde(default)]
    duplicator: Option<DuplicatorConfig>,
    #[serde(default)]
    splitter: Option<SplitterConfig>,
    #[serde(default)]
    combiner: Option<CombinerConfig>,
    #[serde(default)]
    writer: Option<WriterConfig>,
    #[serde(default)]
    consumer: Option<ConsumerConfig>,
    #[serde(default)]
    observer: Option<ObserverConfig>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SplitterConfig {
    input_queue: Option<String>,
    green_input_queue: Option<String>,
    blue_input_queue: Option<String>,
    green_input_queue_env_var: Option<String>,
    blue_input_queue_env_var: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DuplicatorConfig {
    input_queue: Option<String>,
    green_input_queue: Option<String>,
    blue_input_queue: Option<String>,
    green_input_queue_env_var: Option<String>,
    blue_input_queue_env_var: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CombinerConfig {
    output_queue: Option<String>,
    green_output_queue: Option<String>,
    blue_output_queue: Option<String>,
    green_output_queue_env_var: Option<String>,
    blue_output_queue_env_var: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WriterConfig {
    target_queue: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConsumerConfig {
    input_queue: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ObserverConfig {
    #[serde(default)]
    test_id: Option<TestIdSelector>,
    #[serde(default)]
    r#match: Vec<FilterCondition>,
    #[serde(default)]
    notify_path: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActiveRole {
    Duplicator,
    Splitter,
    Combiner,
    Observer,
    Writer,
    Consumer,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TestIdSelector {
    field: Option<String>,
    json_path: Option<String>,
    value: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FilterCondition {
    field: String,
    #[serde(default)]
    equals: Option<String>,
    #[serde(default)]
    matches: Option<String>,
    #[serde(default)]
    json_path: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum AssignmentTarget {
    Green,
    Blue,
    Test,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum AssignmentKind {
    Env,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PropertyAssignment {
    target: AssignmentTarget,
    kind: AssignmentKind,
    name: String,
    value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    container_name: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PluginLifecycleResponse {
    #[serde(default)]
    assignments: Vec<PropertyAssignment>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PluginDrainStatusResponse {
    drained: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
#[allow(dead_code)]
struct WriteRequest {
    test_id: Option<String>,
    payload: serde_json::Value,
    properties: Option<serde_json::Value>,
}

#[derive(Clone)]
struct AppState {
    config: Config,
    roles: Vec<ActiveRole>,
    amqp_url: String,
    testcase_registration_url: String,
    test_container_url: String,
    testcase_verify_path_template: Option<String>,
    inception_point: String,
    blue_green_ref: String,
    mode: Arc<AtomicU8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RuntimeMode {
    Active = 0,
    Draining = 1,
    Idle = 2,
}

impl RuntimeMode {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Draining,
            2 => Self::Idle,
            _ => Self::Active,
        }
    }
}

impl AppState {
    fn runtime_mode(&self) -> RuntimeMode {
        RuntimeMode::from_u8(self.mode.load(Ordering::Relaxed))
    }

    fn set_runtime_mode(&self, mode: RuntimeMode) {
        self.mode.store(mode as u8, Ordering::Relaxed);
    }
}

fn load_config() -> Result<Config> {
    let path = std::env::var("FLUIDBG_CONFIG_PATH")
        .unwrap_or_else(|_| "/etc/fluidbg/config.yaml".to_string());
    let data = std::fs::read_to_string(&path)?;
    Ok(serde_yaml_ng::from_str(&data)?)
}

fn load_roles() -> Vec<ActiveRole> {
    std::env::var("FLUIDBG_ACTIVE_ROLES")
        .unwrap_or_default()
        .split(',')
        .filter_map(|value| match value.trim() {
            "duplicator" => Some(ActiveRole::Duplicator),
            "splitter" => Some(ActiveRole::Splitter),
            "combiner" => Some(ActiveRole::Combiner),
            "observer" => Some(ActiveRole::Observer),
            "writer" => Some(ActiveRole::Writer),
            "consumer" => Some(ActiveRole::Consumer),
            _ => None,
        })
        .collect()
}

fn has_role(roles: &[ActiveRole], role: ActiveRole) -> bool {
    roles.contains(&role)
}

fn required<'a>(value: &'a Option<String>, name: &str) -> Result<&'a str> {
    value
        .as_deref()
        .with_context(|| format!("missing config field '{name}'"))
}

fn duplicator_config(config: &Config) -> Result<&DuplicatorConfig> {
    config
        .duplicator
        .as_ref()
        .context("missing config block 'duplicator'")
}

fn splitter_config(config: &Config) -> Result<&SplitterConfig> {
    config
        .splitter
        .as_ref()
        .context("missing config block 'splitter'")
}

fn combiner_config(config: &Config) -> Result<&CombinerConfig> {
    config
        .combiner
        .as_ref()
        .context("missing config block 'combiner'")
}

fn writer_config(config: &Config) -> Result<&WriterConfig> {
    config
        .writer
        .as_ref()
        .context("missing config block 'writer'")
}

fn consumer_config(config: &Config) -> Result<&ConsumerConfig> {
    config
        .consumer
        .as_ref()
        .context("missing config block 'consumer'")
}

fn observer_config(config: &Config) -> Option<&ObserverConfig> {
    config.observer.as_ref()
}

fn blue_traffic_percent() -> u8 {
    std::env::var("FLUIDBG_TRAFFIC_PERCENT")
        .ok()
        .and_then(|value| value.parse::<u8>().ok())
        .unwrap_or(100)
        .min(100)
}

fn routes_to_blue(payload: &[u8], traffic_percent: u8) -> bool {
    if traffic_percent == 0 {
        return false;
    }
    if traffic_percent >= 100 {
        return true;
    }
    let mut hasher = DefaultHasher::new();
    payload.hash(&mut hasher);
    (hasher.finish() % 100) < traffic_percent as u64
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

async fn declare_queue(channel: &Channel, queue: &str) -> Result<()> {
    channel
        .queue_declare(queue, QueueDeclareOptions::default(), FieldTable::default())
        .await?;
    Ok(())
}

async fn delete_queue(channel: &Channel, queue: &str) -> Result<()> {
    match channel
        .queue_delete(queue, QueueDeleteOptions::default())
        .await
    {
        Ok(_) => Ok(()),
        Err(err) => {
            warn!("queue delete for '{}' failed: {}", queue, err);
            Ok(())
        }
    }
}

fn is_missing_queue_error(err: &lapin::Error) -> bool {
    err.to_string().contains("NOT_FOUND - no queue")
}

async fn queue_state(channel: &Channel, queue: &str) -> Result<(u32, u32)> {
    let declared = match channel
        .queue_declare(
            queue,
            QueueDeclareOptions {
                passive: true,
                ..Default::default()
            },
            FieldTable::default(),
        )
        .await
    {
        Ok(declared) => declared,
        Err(err) if is_missing_queue_error(&err) => return Ok((0, 0)),
        Err(err) => return Err(err.into()),
    };
    Ok((declared.message_count(), declared.consumer_count()))
}

async fn move_queue_messages(channel: &Channel, source_queue: &str, target_queue: &str) -> Result<u32> {
    let mut moved = 0;
    loop {
        let delivery = match channel
            .basic_get(source_queue, BasicGetOptions::default())
            .await
        {
            Ok(delivery) => delivery,
            Err(err) if is_missing_queue_error(&err) => return Ok(moved),
            Err(err) => return Err(err.into()),
        };
        let Some(delivery) = delivery else {
            break;
        };
        let headers = delivery.properties.headers().clone().unwrap_or_default();
        channel
            .basic_publish(
                "",
                target_queue,
                BasicPublishOptions::default(),
                &delivery.data,
                BasicProperties::default().with_headers(headers),
            )
            .await?;
        delivery.ack(BasicAckOptions::default()).await?;
        moved += 1;
    }
    Ok(moved)
}

async fn connect_with_retry(amqp_url: &str) -> Result<Connection> {
    for attempt in 1..=30 {
        match Connection::connect(amqp_url, ConnectionProperties::default()).await {
            Ok(conn) => return Ok(conn),
            Err(err) if attempt < 30 => {
                warn!(
                    "RabbitMQ connection failed (attempt {}/30): {}",
                    attempt, err
                );
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Err(err) => return Err(err.into()),
        }
    }
    bail!("unable to connect to RabbitMQ");
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
    let notification = serde_json::json!({
        "testId": test_id,
        "inceptionPoint": inception_point,
        "payload": payload,
    });
    client
        .post(format!(
            "{}{}",
            test_container_url.trim_end_matches('/'),
            path
        ))
        .json(&notification)
        .send()
        .await?;
    Ok(())
}

async fn register_test_case(
    client: &reqwest::Client,
    testcase_registration_url: &str,
    blue_green_ref: &str,
    inception_point: &str,
    test_id: &str,
    test_container_url: &str,
    testcase_verify_path_template: Option<&str>,
) -> Result<()> {
    let verify_url = testcase_verify_path_template.map(|path| {
        format!(
            "{}{}",
            test_container_url.trim_end_matches('/'),
            path.replace("{testId}", test_id)
        )
    });
    client
        .post(testcase_registration_url)
        .json(&serde_json::json!({
            "test_id": test_id,
            "blue_green_ref": blue_green_ref,
            "inception_point": inception_point,
            "timeout_seconds": 120,
            "verify_url": verify_url,
        }))
        .send()
        .await?
        .error_for_status()?;
    info!(
        "registered testCase '{}' for blueGreenRef '{}' via {}",
        test_id, blue_green_ref, testcase_registration_url
    );
    Ok(())
}

fn build_prepare_assignments(config: &Config, roles: &[ActiveRole]) -> Vec<PropertyAssignment> {
    let mut assignments = Vec::new();
    if has_role(roles, ActiveRole::Duplicator)
        && let Some(duplicator) = config.duplicator.as_ref()
    {
        if let (Some(env_name), Some(queue)) = (
            &duplicator.green_input_queue_env_var,
            &duplicator.green_input_queue,
        ) {
            assignments.push(PropertyAssignment {
                target: AssignmentTarget::Green,
                kind: AssignmentKind::Env,
                name: env_name.clone(),
                value: queue.clone(),
                container_name: None,
            });
        }
        if let (Some(env_name), Some(queue)) = (
            &duplicator.blue_input_queue_env_var,
            &duplicator.blue_input_queue,
        ) {
            assignments.push(PropertyAssignment {
                target: AssignmentTarget::Blue,
                kind: AssignmentKind::Env,
                name: env_name.clone(),
                value: queue.clone(),
                container_name: None,
            });
        }
    }
    if has_role(roles, ActiveRole::Splitter)
        && let Some(splitter) = config.splitter.as_ref()
    {
        if let (Some(env_name), Some(queue)) = (
            &splitter.green_input_queue_env_var,
            &splitter.green_input_queue,
        ) {
            assignments.push(PropertyAssignment {
                target: AssignmentTarget::Green,
                kind: AssignmentKind::Env,
                name: env_name.clone(),
                value: queue.clone(),
                container_name: None,
            });
        }
        if let (Some(env_name), Some(queue)) = (
            &splitter.blue_input_queue_env_var,
            &splitter.blue_input_queue,
        ) {
            assignments.push(PropertyAssignment {
                target: AssignmentTarget::Blue,
                kind: AssignmentKind::Env,
                name: env_name.clone(),
                value: queue.clone(),
                container_name: None,
            });
        }
    }
    if has_role(roles, ActiveRole::Combiner)
        && let Some(combiner) = config.combiner.as_ref()
    {
        if let (Some(env_name), Some(queue)) = (
            &combiner.green_output_queue_env_var,
            &combiner.green_output_queue,
        ) {
            assignments.push(PropertyAssignment {
                target: AssignmentTarget::Green,
                kind: AssignmentKind::Env,
                name: env_name.clone(),
                value: queue.clone(),
                container_name: None,
            });
        }
        if let (Some(env_name), Some(queue)) = (
            &combiner.blue_output_queue_env_var,
            &combiner.blue_output_queue,
        ) {
            assignments.push(PropertyAssignment {
                target: AssignmentTarget::Blue,
                kind: AssignmentKind::Env,
                name: env_name.clone(),
                value: queue.clone(),
                container_name: None,
            });
        }
    }
    assignments
}

fn build_cleanup_assignments(config: &Config, roles: &[ActiveRole]) -> Vec<PropertyAssignment> {
    let mut assignments = Vec::new();
    if has_role(roles, ActiveRole::Duplicator)
        && let Some(duplicator) = config.duplicator.as_ref()
    {
        if let (Some(env_name), Some(queue)) = (
            &duplicator.green_input_queue_env_var,
            &duplicator.input_queue,
        ) {
            assignments.push(PropertyAssignment {
                target: AssignmentTarget::Green,
                kind: AssignmentKind::Env,
                name: env_name.clone(),
                value: queue.clone(),
                container_name: None,
            });
        }
        if let (Some(env_name), Some(queue)) = (
            &duplicator.blue_input_queue_env_var,
            &duplicator.input_queue,
        ) {
            assignments.push(PropertyAssignment {
                target: AssignmentTarget::Blue,
                kind: AssignmentKind::Env,
                name: env_name.clone(),
                value: queue.clone(),
                container_name: None,
            });
        }
    }
    if has_role(roles, ActiveRole::Splitter)
        && let Some(splitter) = config.splitter.as_ref()
    {
        if let (Some(env_name), Some(queue)) = (
            &splitter.green_input_queue_env_var,
            &splitter.input_queue,
        ) {
            assignments.push(PropertyAssignment {
                target: AssignmentTarget::Green,
                kind: AssignmentKind::Env,
                name: env_name.clone(),
                value: queue.clone(),
                container_name: None,
            });
        }
        if let (Some(env_name), Some(queue)) = (
            &splitter.blue_input_queue_env_var,
            &splitter.input_queue,
        ) {
            assignments.push(PropertyAssignment {
                target: AssignmentTarget::Blue,
                kind: AssignmentKind::Env,
                name: env_name.clone(),
                value: queue.clone(),
                container_name: None,
            });
        }
    }
    if has_role(roles, ActiveRole::Combiner)
        && let Some(combiner) = config.combiner.as_ref()
    {
        if let (Some(env_name), Some(queue)) = (
            &combiner.green_output_queue_env_var,
            &combiner.output_queue,
        ) {
            assignments.push(PropertyAssignment {
                target: AssignmentTarget::Green,
                kind: AssignmentKind::Env,
                name: env_name.clone(),
                value: queue.clone(),
                container_name: None,
            });
        }
        if let (Some(env_name), Some(queue)) = (
            &combiner.blue_output_queue_env_var,
            &combiner.output_queue,
        ) {
            assignments.push(PropertyAssignment {
                target: AssignmentTarget::Blue,
                kind: AssignmentKind::Env,
                name: env_name.clone(),
                value: queue.clone(),
                container_name: None,
            });
        }
    }
    assignments
}

fn build_drain_assignments(config: &Config, roles: &[ActiveRole]) -> Vec<PropertyAssignment> {
    build_cleanup_assignments(config, roles)
}

async fn compute_drain_status(state: &AppState) -> Result<PluginDrainStatusResponse> {
    let conn = connect_with_retry(&state.amqp_url).await?;
    let channel = conn.create_channel().await?;

    if has_role(&state.roles, ActiveRole::Duplicator) {
        let config = duplicator_config(&state.config)?;
        let green_queue = required(&config.green_input_queue, "duplicator.greenInputQueue")?;
        let blue_queue = required(&config.blue_input_queue, "duplicator.blueInputQueue")?;
        let (green_messages, green_consumers) = queue_state(&channel, green_queue).await?;
        let (blue_messages, blue_consumers) = queue_state(&channel, blue_queue).await?;
        let drained =
            green_messages == 0 && blue_messages == 0 && green_consumers == 0 && blue_consumers == 0;
        let message = if drained {
            Some("temporary input queues are empty and no consumers remain attached".to_string())
        } else if green_consumers > 0 || blue_consumers > 0 {
            Some("temporary input queues still have active consumers; locks may still exist".to_string())
        } else {
            Some(format!(
                "temporary input queues still contain messages (green={}, blue={})",
                green_messages, blue_messages
            ))
        };
        return Ok(PluginDrainStatusResponse { drained, message });
    }

    if has_role(&state.roles, ActiveRole::Splitter) {
        let config = splitter_config(&state.config)?;
        let green_queue = required(&config.green_input_queue, "splitter.greenInputQueue")?;
        let blue_queue = required(&config.blue_input_queue, "splitter.blueInputQueue")?;
        let (green_messages, green_consumers) = queue_state(&channel, green_queue).await?;
        let (blue_messages, blue_consumers) = queue_state(&channel, blue_queue).await?;
        let drained =
            green_messages == 0 && blue_messages == 0 && green_consumers == 0 && blue_consumers == 0;
        let message = if drained {
            Some("temporary input queues are empty and no consumers remain attached".to_string())
        } else if green_consumers > 0 || blue_consumers > 0 {
            Some("temporary input queues still have active consumers; locks may still exist".to_string())
        } else {
            Some(format!(
                "temporary input queues still contain messages (green={}, blue={})",
                green_messages, blue_messages
            ))
        };
        return Ok(PluginDrainStatusResponse { drained, message });
    }

    if has_role(&state.roles, ActiveRole::Combiner) {
        let config = combiner_config(&state.config)?;
        let green_queue = required(&config.green_output_queue, "combiner.greenOutputQueue")?;
        let blue_queue = required(&config.blue_output_queue, "combiner.blueOutputQueue")?;
        let (green_messages, _) = queue_state(&channel, green_queue).await?;
        let (blue_messages, _) = queue_state(&channel, blue_queue).await?;
        let drained = green_messages == 0 && blue_messages == 0;
        let message = if drained {
            Some("temporary output queues are empty".to_string())
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
        message: Some("no drain-sensitive RabbitMQ roles active".to_string()),
    })
}

async fn prepare_handler(
    State(state): State<AppState>,
) -> Result<Json<PluginLifecycleResponse>, axum::http::StatusCode> {
    state.set_runtime_mode(RuntimeMode::Active);
    let conn = connect_with_retry(&state.amqp_url)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    let channel = conn
        .create_channel()
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    if has_role(&state.roles, ActiveRole::Duplicator) {
        let duplicator = duplicator_config(&state.config)
            .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
        for queue in [
            duplicator.green_input_queue.as_deref(),
            duplicator.blue_input_queue.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            declare_queue(&channel, queue)
                .await
                .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
        }
    }
    if has_role(&state.roles, ActiveRole::Splitter) {
        let splitter = splitter_config(&state.config)
            .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
        for queue in [
            splitter.green_input_queue.as_deref(),
            splitter.blue_input_queue.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            declare_queue(&channel, queue)
                .await
                .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
        }
    }
    if has_role(&state.roles, ActiveRole::Combiner) {
        let combiner = combiner_config(&state.config)
            .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
        for queue in [
            combiner.green_output_queue.as_deref(),
            combiner.blue_output_queue.as_deref(),
            combiner.output_queue.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            declare_queue(&channel, queue)
                .await
                .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
        }
    }
    if has_role(&state.roles, ActiveRole::Writer) {
        let writer = writer_config(&state.config)
            .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
        if let Some(queue) = &writer.target_queue {
        declare_queue(&channel, queue)
            .await
            .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
        }
    }

    Ok(Json(PluginLifecycleResponse {
        assignments: build_prepare_assignments(&state.config, &state.roles),
    }))
}

async fn cleanup_handler(
    State(state): State<AppState>,
) -> Result<Json<PluginLifecycleResponse>, axum::http::StatusCode> {
    state.set_runtime_mode(RuntimeMode::Idle);
    tokio::time::sleep(Duration::from_secs(1)).await;
    let conn = connect_with_retry(&state.amqp_url)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    let channel = conn
        .create_channel()
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    if has_role(&state.roles, ActiveRole::Duplicator) {
        let duplicator = duplicator_config(&state.config)
            .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
        for queue in [
            duplicator.green_input_queue.as_deref(),
            duplicator.blue_input_queue.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            delete_queue(&channel, queue)
                .await
                .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
        }
    }
    if has_role(&state.roles, ActiveRole::Splitter) {
        let splitter = splitter_config(&state.config)
            .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
        for queue in [
            splitter.green_input_queue.as_deref(),
            splitter.blue_input_queue.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            delete_queue(&channel, queue)
                .await
                .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
        }
    }
    if has_role(&state.roles, ActiveRole::Combiner) {
        let combiner = combiner_config(&state.config)
            .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
        for queue in [
            combiner.green_output_queue.as_deref(),
            combiner.blue_output_queue.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            delete_queue(&channel, queue)
                .await
                .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
        }
    }

    Ok(Json(PluginLifecycleResponse {
        assignments: Vec::new(),
    }))
}

async fn drain_handler(
    State(state): State<AppState>,
) -> Result<Json<PluginLifecycleResponse>, axum::http::StatusCode> {
    state.set_runtime_mode(RuntimeMode::Draining);
    Ok(Json(PluginLifecycleResponse {
        assignments: build_drain_assignments(&state.config, &state.roles),
    }))
}

async fn drain_status_handler(
    State(state): State<AppState>,
) -> Result<Json<PluginDrainStatusResponse>, axum::http::StatusCode> {
    let status = compute_drain_status(&state)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(status))
}

async fn health() -> &'static str {
    "ok"
}

async fn write_handler(
    State(state): State<AppState>,
    Json(req): Json<WriteRequest>,
) -> impl axum::response::IntoResponse {
    let Some(queue) = state
        .config
        .writer
        .as_ref()
        .and_then(|writer| writer.target_queue.clone())
    else {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            "targetQueue not configured",
        );
    };

    let conn = match connect_with_retry(&state.amqp_url).await {
        Ok(conn) => conn,
        Err(err) => {
            tracing::error!("failed to connect for write: {}", err);
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "connect failed",
            );
        }
    };
    let channel = match conn.create_channel().await {
        Ok(channel) => channel,
        Err(err) => {
            tracing::error!("failed to create write channel: {}", err);
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "channel failed",
            );
        }
    };
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

async fn process_input_delivery(
    state: &AppState,
    channel: &Channel,
    delivery: lapin::message::BasicGetMessage,
    http_client: &reqwest::Client,
) -> Result<()> {
    let body_data = delivery.data.clone();
    let body_str = String::from_utf8_lossy(&body_data);
    let body_json: Value = serde_json::from_str(&body_str).unwrap_or(Value::Null);
    let headers = delivery.properties.headers().clone().unwrap_or_default();

    let observer = observer_config(&state.config);
    let filters = observer.map(|o| o.r#match.as_slice()).unwrap_or(&[]);
    if !matches_filter(filters, &body_json, &headers) {
        delivery.ack(BasicAckOptions::default()).await?;
        return Ok(());
    }

    if has_role(&state.roles, ActiveRole::Duplicator) {
        let duplicator = duplicator_config(&state.config)?;
        if let Some(green_queue) = &duplicator.green_input_queue {
            channel
                .basic_publish(
                    "",
                    green_queue,
                    BasicPublishOptions::default(),
                    &body_data,
                    BasicProperties::default().with_headers(headers.clone()),
                )
                .await?;
        }
        if let Some(blue_queue) = &duplicator.blue_input_queue {
            channel
                .basic_publish(
                    "",
                    blue_queue,
                    BasicPublishOptions::default(),
                    &body_data,
                    BasicProperties::default().with_headers(headers.clone()),
                )
                .await?;
        }
    } else if has_role(&state.roles, ActiveRole::Splitter) {
        let splitter = splitter_config(&state.config)?;
        let send_to_blue = routes_to_blue(&body_data, blue_traffic_percent());
        if send_to_blue {
            if let Some(blue_queue) = &splitter.blue_input_queue {
                channel
                    .basic_publish(
                        "",
                        blue_queue,
                        BasicPublishOptions::default(),
                        &body_data,
                        BasicProperties::default().with_headers(headers.clone()),
                    )
                    .await?;
            }
        } else if let Some(green_queue) = &splitter.green_input_queue {
            channel
                .basic_publish(
                    "",
                    green_queue,
                    BasicPublishOptions::default(),
                    &body_data,
                    BasicProperties::default().with_headers(headers.clone()),
                )
                .await?;
        }
    }

    if has_role(&state.roles, ActiveRole::Observer)
        && let Some(observer) = observer
        && let Some(selector) = &observer.test_id
        && let Some(test_id) = extract_test_id(selector, &body_json, &headers)
    {
        if let Err(err) = register_test_case(
            http_client,
            &state.testcase_registration_url,
            &state.blue_green_ref,
            &state.inception_point,
            &test_id,
            &state.test_container_url,
            state.testcase_verify_path_template.as_deref(),
        )
        .await
        {
            warn!("failed to register test case {}: {}", test_id, err);
        }
        if let Some(notify_path) = &observer.notify_path
            && let Err(err) = notify_test_container(
                http_client,
                &state.test_container_url,
                notify_path,
                &test_id,
                &state.inception_point,
                &body_json,
            )
            .await
        {
            warn!("failed to notify test container for {}: {}", test_id, err);
        }
    }

    delivery.ack(BasicAckOptions::default()).await?;
    Ok(())
}

async fn drain_input_queues(state: &AppState, channel: &Channel) -> Result<()> {
    let (base_queue, green_queue, blue_queue) = if has_role(&state.roles, ActiveRole::Duplicator) {
        let config = duplicator_config(&state.config)?;
        (
            required(&config.input_queue, "duplicator.inputQueue")?.to_string(),
            required(&config.green_input_queue, "duplicator.greenInputQueue")?.to_string(),
            required(&config.blue_input_queue, "duplicator.blueInputQueue")?.to_string(),
        )
    } else if has_role(&state.roles, ActiveRole::Splitter) {
        let config = splitter_config(&state.config)?;
        (
            required(&config.input_queue, "splitter.inputQueue")?.to_string(),
            required(&config.green_input_queue, "splitter.greenInputQueue")?.to_string(),
            required(&config.blue_input_queue, "splitter.blueInputQueue")?.to_string(),
        )
    } else {
        return Ok(());
    };

    declare_queue(channel, &base_queue).await?;
    let (_, green_consumers) = queue_state(channel, &green_queue).await?;
    let (_, blue_consumers) = queue_state(channel, &blue_queue).await?;

    if green_consumers == 0 {
        let moved = move_queue_messages(channel, &green_queue, &base_queue).await?;
        if moved > 0 {
            info!(
                "moved {} message(s) from {} back to {} during drain",
                moved, green_queue, base_queue
            );
        }
    }
    if blue_consumers == 0 {
        let moved = move_queue_messages(channel, &blue_queue, &base_queue).await?;
        if moved > 0 {
            info!(
                "moved {} message(s) from {} back to {} during drain",
                moved, blue_queue, base_queue
            );
        }
    }

    Ok(())
}

async fn run_input_pipeline(state: AppState) -> Result<()> {
    loop {
        if matches!(state.runtime_mode(), RuntimeMode::Idle) {
            tokio::time::sleep(Duration::from_millis(300)).await;
            continue;
        }
        let conn = match connect_with_retry(&state.amqp_url).await {
            Ok(conn) => conn,
            Err(err) => {
                warn!("rabbitmq input pipeline connect failed, reconnecting: {}", err);
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        let channel = match conn.create_channel().await {
            Ok(channel) => channel,
            Err(err) => {
                warn!("rabbitmq input pipeline channel failed, reconnecting: {}", err);
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        let input_queue = if has_role(&state.roles, ActiveRole::Duplicator) {
            required(
                &duplicator_config(&state.config)?.input_queue,
                "duplicator.inputQueue",
            )?
            .to_string()
        } else if has_role(&state.roles, ActiveRole::Splitter) {
            required(&splitter_config(&state.config)?.input_queue, "splitter.inputQueue")?
                .to_string()
        } else {
            required(&consumer_config(&state.config)?.input_queue, "consumer.inputQueue")?
                .to_string()
        };
        declare_queue(&channel, &input_queue).await?;
        let http_client = reqwest::Client::new();
        info!("rabbitmq input pipeline polling {}", input_queue);

        loop {
            match state.runtime_mode() {
                RuntimeMode::Idle => {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    continue;
                }
                RuntimeMode::Draining => {
                    if let Err(err) = drain_input_queues(&state, &channel).await {
                        warn!("rabbitmq input drain failed, reconnecting: {}", err);
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    continue;
                }
                RuntimeMode::Active => {}
            }

            match channel
                .basic_get(&input_queue, BasicGetOptions::default())
                .await
            {
                Ok(Some(delivery)) => {
                    if let Err(err) =
                        process_input_delivery(&state, &channel, delivery, &http_client).await
                    {
                        warn!("rabbitmq input processing failed, reconnecting: {}", err);
                        break;
                    }
                }
                Ok(None) => {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                Err(err) => {
                    warn!("rabbitmq input poll failed, reconnecting: {}", err);
                    break;
                }
            }
        }

        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn run_combine_loop_once(
    source_queue: String,
    result_queue: String,
    state: AppState,
) -> Result<()> {
    let conn = connect_with_retry(&state.amqp_url).await?;
    let consume_channel = conn.create_channel().await?;
    let publish_channel = conn.create_channel().await?;
    declare_queue(&consume_channel, &source_queue).await?;
    declare_queue(&publish_channel, &result_queue).await?;
    let http_client = reqwest::Client::new();

    let mut consumer = consume_channel
        .basic_consume(
            &source_queue,
            "fluidbg-rabbitmq-combiner",
            BasicConsumeOptions::default(),
            FieldTable::default(),
        )
        .await?;

    while let Some(delivery) = consumer.next().await {
        match state.runtime_mode() {
            RuntimeMode::Idle => {
                tokio::time::sleep(Duration::from_millis(300)).await;
                continue;
            }
            RuntimeMode::Draining => {
                tokio::time::sleep(Duration::from_millis(300)).await;
                continue;
            }
            RuntimeMode::Active => {}
        }
        let delivery = delivery?;
        let headers = delivery.properties.headers().clone().unwrap_or_default();
        let body_json: Value = serde_json::from_slice(&delivery.data).unwrap_or(Value::Null);
        publish_channel
            .basic_publish(
                "",
                &result_queue,
                BasicPublishOptions::default(),
                &delivery.data,
                BasicProperties::default().with_headers(headers.clone()),
            )
            .await?;

        if has_role(&state.roles, ActiveRole::Observer)
            && let Some(observer) = observer_config(&state.config)
            && matches_filter(&observer.r#match, &body_json, &headers)
            && let Some(selector) = &observer.test_id
            && let Some(test_id) = extract_test_id(selector, &body_json, &headers)
        {
            if let Err(err) = register_test_case(
                &http_client,
                &state.testcase_registration_url,
                &state.blue_green_ref,
                &state.inception_point,
                &test_id,
                &state.test_container_url,
                state.testcase_verify_path_template.as_deref(),
            )
            .await
            {
                warn!("failed to register test case {}: {}", test_id, err);
            }
            if let Some(notify_path) = &observer.notify_path
                && let Err(err) = notify_test_container(
                    &http_client,
                    &state.test_container_url,
                    notify_path,
                    &test_id,
                    &state.inception_point,
                    &body_json,
                )
                .await
            {
                warn!(
                    "failed to notify test container for {} from {}: {}",
                    test_id, source_queue, err
                );
            }
        }
        delivery.ack(BasicAckOptions::default()).await?;
    }

    Ok(())
}

async fn run_combine_loop(source_queue: String, result_queue: String, state: AppState) -> Result<()> {
    loop {
        if matches!(state.runtime_mode(), RuntimeMode::Idle) {
            tokio::time::sleep(Duration::from_millis(300)).await;
            continue;
        }
        match run_combine_loop_once(source_queue.clone(), result_queue.clone(), state.clone()).await {
            Ok(()) => {
                warn!(
                    "rabbitmq combine loop for '{}' ended, reconnecting",
                    source_queue
                );
            }
            Err(err) => {
                warn!(
                    "rabbitmq combine loop for '{}' failed, reconnecting: {}",
                    source_queue, err
                );
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn run_combiner(state: AppState) -> Result<()> {
    let config = state.config.clone();
    let combiner = combiner_config(&config)?;
    let blue_queue = required(&combiner.blue_output_queue, "combiner.blueOutputQueue")?.to_string();
    let green_queue =
        required(&combiner.green_output_queue, "combiner.greenOutputQueue")?.to_string();
    let result_queue = required(&combiner.output_queue, "combiner.outputQueue")?.to_string();

    let blue_task = tokio::spawn(run_combine_loop(
        blue_queue,
        result_queue.clone(),
        state.clone(),
    ));
    let green_task = tokio::spawn(run_combine_loop(green_queue, result_queue, state));
    let (blue_result, green_result) = tokio::join!(blue_task, green_task);
    blue_result??;
    green_result??;
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
    let roles = load_roles();
    if roles.is_empty() {
        bail!("no active roles configured via FLUIDBG_ACTIVE_ROLES");
    }

    let amqp_url = config
        .amqp_url
        .clone()
        .unwrap_or_else(|| "amqp://fluidbg:fluidbg@rabbitmq.fluidbg-system:5672/%2f".to_string());
    let testcase_registration_url =
        std::env::var("FLUIDBG_TESTCASE_REGISTRATION_URL")
            .unwrap_or_else(|_| "http://localhost:8090/testcases".to_string());
    let test_container_url = std::env::var("FLUIDBG_TEST_CONTAINER_URL")
        .unwrap_or_else(|_| "http://localhost:8080".to_string());
    let testcase_verify_path_template =
        std::env::var("FLUIDBG_TESTCASE_VERIFY_PATH_TEMPLATE").ok();
    let inception_point =
        std::env::var("FLUIDBG_INCEPTION_POINT").unwrap_or_else(|_| "unknown".to_string());
    let blue_green_ref =
        std::env::var("FLUIDBG_BLUE_GREEN_REF").unwrap_or_else(|_| "unknown".to_string());

    let state = AppState {
        config: config.clone(),
        roles: roles.clone(),
        amqp_url: amqp_url.clone(),
        testcase_registration_url,
        test_container_url,
        testcase_verify_path_template,
        inception_point,
        blue_green_ref,
        mode: Arc::new(AtomicU8::new(RuntimeMode::Active as u8)),
    };

    info!("rabbitmq plugin starting with roles {:?}", roles);

    let app = Router::new()
        .route("/health", get(health))
        .route("/prepare", post(prepare_handler))
        .route("/drain", post(drain_handler))
        .route("/drain-status", get(drain_status_handler))
        .route("/cleanup", post(cleanup_handler))
        .route("/write", post(write_handler))
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind("0.0.0.0:9090").await?;
    let server = tokio::spawn(async move {
        if let Err(err) = axum::serve(listener, app).await {
            tracing::error!("server error: {}", err);
        }
    });

    let worker = if has_role(&roles, ActiveRole::Writer) {
        tokio::spawn(async { Ok::<(), anyhow::Error>(()) })
    } else if has_role(&roles, ActiveRole::Combiner) {
        tokio::spawn(async move { run_combiner(state).await })
    } else if has_role(&roles, ActiveRole::Duplicator)
        || has_role(&roles, ActiveRole::Splitter)
        || has_role(&roles, ActiveRole::Observer)
        || has_role(&roles, ActiveRole::Consumer)
    {
        tokio::spawn(async move { run_input_pipeline(state).await })
    } else {
        bail!("unsupported RabbitMQ role set");
    };

    let (_, worker_result) = tokio::join!(server, worker);
    worker_result??;
    Ok(())
}
