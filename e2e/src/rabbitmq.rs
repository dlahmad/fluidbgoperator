use std::process::Child;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use reqwest::Client;
use serde_json::Value;

use crate::command;

pub struct RabbitMq {
    namespace: String,
    port: u16,
    client: Client,
    port_forward: Option<Child>,
}

impl RabbitMq {
    pub fn new(namespace: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            port: random_local_port(),
            client: Client::new(),
            port_forward: None,
        }
    }

    pub async fn ensure_management_port_forward(&mut self) -> Result<()> {
        if self
            .port_forward
            .as_mut()
            .is_some_and(|child| child.try_wait().ok().flatten().is_none())
            && self.health().await.is_ok()
        {
            return Ok(());
        }

        self.stop_port_forward();
        let args = vec![
            "port-forward".to_string(),
            "svc/rabbitmq".to_string(),
            format!("{}:15672", self.port),
            "-n".to_string(),
            self.namespace.clone(),
        ];
        self.port_forward = Some(command::spawn_silent("kubectl", &args)?);
        tokio::time::sleep(Duration::from_secs(3)).await;
        self.wait_http("rabbitmq-management").await
    }

    pub async fn publish(&mut self, routing_key: &str, payload: &str) -> Result<()> {
        let request = serde_json::json!({
            "properties": {},
            "routing_key": routing_key,
            "payload": payload,
            "payload_encoding": "string"
        });

        for i in 1..=10 {
            self.ensure_management_port_forward().await?;
            let response = self
                .client
                .post(format!(
                    "{}/api/exchanges/%2F/amq.default/publish",
                    self.url()
                ))
                .basic_auth("fluidbg", Some("fluidbg"))
                .json(&request)
                .send()
                .await;
            if let Ok(response) = response
                && response.status().is_success()
                && response
                    .json::<Value>()
                    .await
                    .ok()
                    .and_then(|value| value.get("routed").cloned())
                    == Some(Value::Bool(true))
            {
                return Ok(());
            }
            eprintln!("RabbitMQ publish to '{routing_key}' was not routed, retrying ({i}/10)");
            tokio::time::sleep(Duration::from_secs(1)).await;
        }

        bail!("RabbitMQ publish to '{routing_key}' was not routed after retries")
    }

    pub async fn queue_messages(&mut self, queue: &str, count: u32) -> Result<Vec<Value>> {
        self.ensure_management_port_forward().await?;
        let response = self
            .client
            .post(format!("{}/api/queues/%2F/{}/get", self.url(), queue))
            .basic_auth("fluidbg", Some("fluidbg"))
            .json(&serde_json::json!({
                "count": count,
                "ackmode": "ack_requeue_true",
                "encoding": "auto",
                "truncate": 50000
            }))
            .send()
            .await
            .with_context(|| format!("RabbitMQ get messages from {queue}"))?;
        if !response.status().is_success() {
            bail!(
                "RabbitMQ get messages from {queue} failed with {}",
                response.status()
            );
        }
        response
            .json()
            .await
            .context("RabbitMQ queue get response JSON")
    }

    pub async fn assert_queue_drained(&mut self, queue: &str) -> Result<()> {
        let Some(depth) = self.queue_depth(queue).await? else {
            return Ok(());
        };
        if depth.ready == 0 && depth.unacknowledged == 0 {
            Ok(())
        } else {
            bail!(
                "queue {queue} not drained via management API: ready={} unacked={} consumers={}",
                depth.ready,
                depth.unacknowledged,
                depth.consumers
            )
        }
    }

    pub async fn wait_for_consumers(
        &mut self,
        queue: &str,
        min_consumers: u64,
        timeout: Duration,
    ) -> Result<()> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Some(depth) = self.queue_depth(queue).await?
                && depth.consumers >= min_consumers
            {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                let consumers = self
                    .queue_depth(queue)
                    .await?
                    .map(|depth| depth.consumers)
                    .unwrap_or(0);
                bail!(
                    "queue {queue} did not reach {min_consumers} consumer(s) before timeout; current consumers={consumers}"
                );
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    pub async fn queue_contains_processed_message(
        &mut self,
        queue: &str,
        recovery_token: &str,
        instance_prefix: &str,
    ) -> Result<bool> {
        let messages = self.queue_messages(queue, 100).await?;
        Ok(messages
            .iter()
            .any(|message| processed_message_matches(message, recovery_token, instance_prefix)))
    }

    pub async fn queue_contains_json_field(
        &mut self,
        queue: &str,
        field: &str,
        expected: &str,
    ) -> Result<bool> {
        let messages = self.queue_messages(queue, 100).await?;
        Ok(messages
            .iter()
            .any(|message| json_payload_field_matches(message, field, expected)))
    }

    async fn queue_depth(&mut self, queue: &str) -> Result<Option<QueueDepth>> {
        self.ensure_management_port_forward().await?;
        let response = self
            .client
            .get(format!("{}/api/queues/%2F/{}", self.url(), queue))
            .basic_auth("fluidbg", Some("fluidbg"))
            .send()
            .await
            .with_context(|| format!("RabbitMQ queue depth for {queue}"))?;
        if response.status().as_u16() == 404 {
            return Ok(None);
        }
        if !response.status().is_success() {
            bail!(
                "RabbitMQ queue depth for {queue} failed with {}",
                response.status()
            );
        }
        let payload: Value = response.json().await.context("RabbitMQ queue depth JSON")?;
        Ok(Some(QueueDepth {
            ready: payload
                .get("messages_ready")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            unacknowledged: payload
                .get("messages_unacknowledged")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            consumers: payload
                .get("consumers")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        }))
    }

    async fn health(&self) -> Result<()> {
        let response = self.client.get(self.url()).send().await?;
        if response.status().is_success() {
            Ok(())
        } else {
            bail!("RabbitMQ management returned {}", response.status())
        }
    }

    async fn wait_http(&self, name: &str) -> Result<()> {
        for i in 1..=30 {
            if self.health().await.is_ok() {
                return Ok(());
            }
            eprintln!("waiting for {name} ({i}/30)");
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        bail!("{name} did not become ready")
    }

    fn url(&self) -> String {
        format!("http://localhost:{}", self.port)
    }

    fn stop_port_forward(&mut self) {
        if let Some(mut child) = self.port_forward.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for RabbitMq {
    fn drop(&mut self) {
        self.stop_port_forward();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct QueueDepth {
    ready: u64,
    unacknowledged: u64,
    consumers: u64,
}

fn random_local_port() -> u16 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.subsec_nanos())
        .unwrap_or(0);
    25_000 + (nanos % 20_000) as u16
}

fn processed_message_matches(message: &Value, recovery_token: &str, instance_prefix: &str) -> bool {
    let Some(payload) = message.get("payload").and_then(Value::as_str) else {
        return false;
    };
    let Ok(decoded) = serde_json::from_str::<Value>(payload) else {
        return false;
    };
    let token_matches = decoded
        .get("originalMessage")
        .and_then(|original| original.get("recoveryToken"))
        .and_then(Value::as_str)
        == Some(recovery_token);
    let instance_matches = decoded
        .get("instanceName")
        .and_then(Value::as_str)
        .is_some_and(|name| name.starts_with(&format!("{instance_prefix}-")));
    token_matches && instance_matches
}

fn json_payload_field_matches(message: &Value, field: &str, expected: &str) -> bool {
    let Some(payload) = message.get("payload").and_then(Value::as_str) else {
        return false;
    };
    let Ok(decoded) = serde_json::from_str::<Value>(payload) else {
        return false;
    };
    decoded
        .get(field)
        .map(|value| {
            value
                .as_str()
                .map(ToString::to_string)
                .unwrap_or_else(|| value.to_string())
        })
        .as_deref()
        == Some(expected)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn detects_recovered_processed_message_by_token_and_instance() {
        let message = json!({
            "payload": r#"{"originalMessage":{"recoveryToken":"rollback-1"},"instanceName":"order-processor-abc"}"#
        });

        assert!(processed_message_matches(
            &message,
            "rollback-1",
            "order-processor"
        ));
        assert!(!processed_message_matches(
            &message,
            "other",
            "order-processor"
        ));
        assert!(!processed_message_matches(&message, "rollback-1", "wrong"));
    }

    #[test]
    fn detects_shadow_recovery_payload_fields() {
        let message = json!({
            "payload": r#"{"shadowToken":"shadow-1","type":"order"}"#
        });

        assert!(json_payload_field_matches(
            &message,
            "shadowToken",
            "shadow-1"
        ));
        assert!(!json_payload_field_matches(
            &message,
            "shadowToken",
            "shadow-2"
        ));
    }
}
