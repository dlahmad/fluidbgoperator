use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::kube::{Kube, PodHttpRequest};

pub struct RabbitMq {
    namespace: String,
    kube: Kube,
}

impl RabbitMq {
    pub fn new(namespace: impl Into<String>, kube: Kube) -> Self {
        Self {
            namespace: namespace.into(),
            kube,
        }
    }

    pub async fn publish(&mut self, routing_key: &str, payload: &str) -> Result<()> {
        self.wait_queue_exists(routing_key, Duration::from_secs(30))
            .await?;
        let request = serde_json::json!({
            "properties": {},
            "routing_key": routing_key,
            "payload": payload,
            "payload_encoding": "string"
        });

        for i in 1..=10 {
            let response = self
                .kube
                .pod_http_by_selector(
                    &self.namespace,
                    "app=rabbitmq",
                    15672,
                    PodHttpRequest {
                        method: "POST",
                        path: "/api/exchanges/%2F/amq.default/publish",
                        basic_auth: Some(("fluidbg", "fluidbg")),
                        headers: Vec::new(),
                        body: Some(request.clone()),
                    },
                )
                .await;
            if let Ok(response) = response {
                if !(200..300).contains(&response.status) {
                    eprintln!(
                        "RabbitMQ publish to '{routing_key}' returned HTTP {}, retrying ({i}/10)",
                        response.status
                    );
                } else {
                    match serde_json::from_slice::<Value>(&response.body) {
                        Ok(body) if body.get("routed") == Some(&Value::Bool(true)) => {
                            return Ok(());
                        }
                        Ok(body) => {
                            eprintln!(
                                "RabbitMQ publish to '{routing_key}' returned unexpected body {body}, retrying ({i}/10)"
                            );
                        }
                        Err(error) => {
                            eprintln!(
                                "RabbitMQ publish to '{routing_key}' returned non-JSON body: {error}, retrying ({i}/10)"
                            );
                        }
                    }
                }
            }
            eprintln!("RabbitMQ publish to '{routing_key}' was not routed, retrying ({i}/10)");
            tokio::time::sleep(Duration::from_secs(1)).await;
        }

        bail!("RabbitMQ publish to '{routing_key}' was not routed after retries")
    }

    async fn wait_queue_exists(&mut self, queue: &str, timeout: Duration) -> Result<()> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if self.queue_depth(queue).await?.is_some() {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                bail!("queue {queue} did not exist before publish timeout");
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    pub async fn queue_messages(&mut self, queue: &str, count: u32) -> Result<Vec<Value>> {
        let response = self
            .rabbitmq_json(PodHttpRequest {
                method: "POST",
                path: &format!("/api/queues/%2F/{queue}/get"),
                basic_auth: Some(("fluidbg", "fluidbg")),
                headers: Vec::new(),
                body: Some(serde_json::json!({
                    "count": count,
                    "ackmode": "ack_requeue_true",
                    "encoding": "auto",
                    "truncate": 50000
                })),
            })
            .await
            .with_context(|| format!("RabbitMQ get messages from {queue}"))?;
        serde_json::from_value(response).context("RabbitMQ queue get response JSON")
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
        let response = self
            .kube
            .pod_http_by_selector(
                &self.namespace,
                "app=rabbitmq",
                15672,
                PodHttpRequest {
                    method: "GET",
                    path: &format!("/api/queues/%2F/{queue}"),
                    basic_auth: Some(("fluidbg", "fluidbg")),
                    headers: Vec::new(),
                    body: None,
                },
            )
            .await
            .with_context(|| format!("RabbitMQ queue depth for {queue}"))?;
        if response.status == 404 {
            return Ok(None);
        }
        if !(200..300).contains(&response.status) {
            bail!(
                "RabbitMQ queue depth for {queue} failed with {}",
                response.status
            );
        }
        let payload: Value =
            serde_json::from_slice(&response.body).context("RabbitMQ queue depth JSON")?;
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

    async fn rabbitmq_json(&self, request: PodHttpRequest<'_>) -> Result<Value> {
        self.kube
            .pod_http_json_by_selector(&self.namespace, "app=rabbitmq", 15672, request)
            .await
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct QueueDepth {
    ready: u64,
    unacknowledged: u64,
    consumers: u64,
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
