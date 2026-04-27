use anyhow::{Context, Result};
use serde::Deserialize;

use crate::config::{Config, ManagementConfig};

#[derive(Clone)]
pub(crate) struct ManagementClient {
    http: reqwest::Client,
    base_url: String,
    username: String,
    password: String,
    vhost: String,
}

#[derive(Clone, Debug)]
pub(crate) struct QueueDepth {
    pub(crate) ready: u64,
    pub(crate) unacknowledged: u64,
    pub(crate) consumers: u64,
}

#[derive(Deserialize)]
struct QueueResponse {
    #[serde(default)]
    messages_ready: u64,
    #[serde(default)]
    messages_unacknowledged: u64,
    #[serde(default)]
    consumers: u64,
}

impl ManagementClient {
    pub(crate) fn from_config(config: &Config) -> Option<Self> {
        let management = config.management.as_ref()?;
        Self::from_management_config(management)
    }

    fn from_management_config(management: &ManagementConfig) -> Option<Self> {
        let url = management
            .url
            .clone()
            .or_else(|| std::env::var("FLUIDBG_RABBITMQ_MANAGEMENT_URL").ok())?;
        Some(Self {
            http: reqwest::Client::new(),
            base_url: url.trim_end_matches('/').to_string(),
            username: management
                .username
                .clone()
                .or_else(|| std::env::var("FLUIDBG_RABBITMQ_MANAGEMENT_USERNAME").ok())
                .unwrap_or_else(|| "guest".to_string()),
            password: management
                .password
                .clone()
                .or_else(|| std::env::var("FLUIDBG_RABBITMQ_MANAGEMENT_PASSWORD").ok())
                .unwrap_or_else(|| "guest".to_string()),
            vhost: management
                .vhost
                .clone()
                .or_else(|| std::env::var("FLUIDBG_RABBITMQ_MANAGEMENT_VHOST").ok())
                .unwrap_or_else(|| "/".to_string()),
        })
    }

    pub(crate) async fn queue_depth(&self, queue: &str) -> Result<QueueDepth> {
        let url = format!(
            "{}/api/queues/{}/{}",
            self.base_url,
            encode_path_segment(&self.vhost),
            encode_path_segment(queue)
        );
        let response = self
            .http
            .get(&url)
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await
            .with_context(|| format!("failed to query RabbitMQ management API for '{queue}'"))?;
        if response.status().as_u16() == 404 {
            return Ok(QueueDepth {
                ready: 0,
                unacknowledged: 0,
                consumers: 0,
            });
        }
        let response = response.error_for_status()?;
        let queue = response.json::<QueueResponse>().await?;
        Ok(QueueDepth {
            ready: queue.messages_ready,
            unacknowledged: queue.messages_unacknowledged,
            consumers: queue.consumers,
        })
    }
}

fn encode_path_segment(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.as_bytes() {
        match *byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(*byte as char)
            }
            byte => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_default_vhost_for_management_api_path() {
        assert_eq!(encode_path_segment("/"), "%2F");
        assert_eq!(encode_path_segment("orders/v1"), "orders%2Fv1");
    }
}
