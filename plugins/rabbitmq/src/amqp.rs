use std::time::Duration;

use anyhow::{Result, bail};
use lapin::options::{
    BasicAckOptions, BasicGetOptions, BasicPublishOptions, QueueDeclareOptions, QueueDeleteOptions,
};
use lapin::types::FieldTable;
use lapin::{BasicProperties, Channel, Connection, ConnectionProperties};
use tracing::warn;

pub(crate) async fn declare_queue(channel: &Channel, queue: &str) -> Result<()> {
    channel
        .queue_declare(
            queue.into(),
            QueueDeclareOptions::default(),
            FieldTable::default(),
        )
        .await?;
    Ok(())
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
        let headers = delivery.properties.headers().clone().unwrap_or_default();
        publish_confirmed(
            channel,
            target_queue,
            &delivery.data,
            BasicProperties::default().with_headers(headers),
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
