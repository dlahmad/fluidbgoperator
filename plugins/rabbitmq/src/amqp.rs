use std::time::Duration;

use anyhow::{Context, Result, bail};
use lapin::options::{
    BasicAckOptions, BasicGetOptions, BasicPublishOptions, QueueDeclareOptions, QueueDeleteOptions,
};
use lapin::types::{AMQPValue, FieldTable, LongString, ShortString};
use lapin::{BasicProperties, Channel, Connection, ConnectionProperties};
use serde_json::Value;
use tracing::warn;

use crate::config::QueueDeclarationConfig;

pub(crate) async fn declare_queue(
    channel: &Channel,
    queue: &str,
    declaration: &QueueDeclarationConfig,
) -> Result<()> {
    channel
        .queue_declare(
            queue.into(),
            QueueDeclareOptions {
                passive: false,
                durable: declaration.durable.unwrap_or(false),
                exclusive: declaration.exclusive.unwrap_or(false),
                auto_delete: declaration.auto_delete.unwrap_or(false),
                nowait: false,
            },
            queue_arguments(declaration)?,
        )
        .await?;
    Ok(())
}

fn queue_arguments(declaration: &QueueDeclarationConfig) -> Result<FieldTable> {
    let mut table = FieldTable::default();
    for (key, value) in &declaration.arguments {
        table.insert(
            ShortString::from(key.as_str()),
            json_to_amqp_value(value).with_context(|| {
                format!("unsupported RabbitMQ queue argument '{key}' value {value}")
            })?,
        );
    }
    Ok(table)
}

fn json_to_amqp_value(value: &Value) -> Option<AMQPValue> {
    match value {
        Value::Bool(value) => Some(AMQPValue::Boolean(*value)),
        Value::Number(value) => value
            .as_i64()
            .map(|value| {
                i32::try_from(value)
                    .map(AMQPValue::LongInt)
                    .unwrap_or(AMQPValue::LongLongInt(value))
            })
            .or_else(|| {
                value.as_u64().map(|value| {
                    u32::try_from(value)
                        .map(AMQPValue::LongUInt)
                        .unwrap_or(AMQPValue::LongLongInt(value.min(i64::MAX as u64) as i64))
                })
            })
            .or_else(|| value.as_f64().map(AMQPValue::Double)),
        Value::String(value) => Some(AMQPValue::LongString(LongString::from(value.as_str()))),
        Value::Null => Some(AMQPValue::Void),
        Value::Array(_) | Value::Object(_) => None,
    }
}

pub(crate) async fn delete_queue(channel: &Channel, queue: &str) -> Result<()> {
    match channel
        .queue_delete(queue.into(), QueueDeleteOptions::default())
        .await
    {
        Ok(_) => Ok(()),
        Err(err) => {
            warn!("queue delete for '{}' failed: {}", queue, err);
            Ok(())
        }
    }
}

pub(crate) async fn publish_confirmed(
    channel: &Channel,
    routing_key: &str,
    payload: &[u8],
    properties: BasicProperties,
) -> Result<()> {
    channel
        .basic_publish(
            "".into(),
            routing_key.into(),
            BasicPublishOptions::default(),
            payload,
            properties,
        )
        .await?
        .await?;
    Ok(())
}

fn is_missing_queue_error(err: &lapin::Error) -> bool {
    err.to_string().contains("NOT_FOUND - no queue")
}

pub(crate) async fn queue_state(channel: &Channel, queue: &str) -> Result<(u32, u32)> {
    let declared = match channel
        .queue_declare(
            queue.into(),
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

pub(crate) async fn move_queue_messages(
    channel: &Channel,
    source_queue: &str,
    target_queue: &str,
) -> Result<u32> {
    let mut moved = 0;
    loop {
        let delivery = match channel
            .basic_get(source_queue.into(), BasicGetOptions::default())
            .await
        {
            Ok(delivery) => delivery,
            Err(err) if is_missing_queue_error(&err) => return Ok(moved),
            Err(err) => return Err(err.into()),
        };
        let Some(delivery) = delivery else {
            break;
        };
        publish_confirmed(
            channel,
            target_queue,
            &delivery.data,
            delivery.properties.clone(),
        )
        .await?;
        delivery.ack(BasicAckOptions::default()).await?;
        moved += 1;
    }
    Ok(moved)
}

pub(crate) async fn connect_with_retry(amqp_url: &str) -> Result<Connection> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn queue_arguments_include_user_properties() {
        let declaration: QueueDeclarationConfig = serde_json::from_value(json!({
            "durable": true,
            "arguments": {
                "x-message-ttl": 60000,
                "x-queue-type": "quorum",
                "x-single-active-consumer": true
            }
        }))
        .unwrap();

        let table = queue_arguments(&declaration).unwrap();

        assert!(table.contains_key("x-message-ttl"));
        assert!(table.contains_key("x-queue-type"));
        assert!(table.contains_key("x-single-active-consumer"));
    }

    #[test]
    fn queue_arguments_reject_nested_json_values() {
        let declaration: QueueDeclarationConfig = serde_json::from_value(json!({
            "arguments": {
                "x-invalid": {"nested": true}
            }
        }))
        .unwrap();

        assert!(queue_arguments(&declaration).is_err());
    }
}
