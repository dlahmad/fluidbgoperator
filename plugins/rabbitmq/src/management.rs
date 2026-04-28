use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::json;

use crate::config::Config;

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

#[derive(Deserialize)]
struct QueueListResponse {
    name: String,
}

#[derive(Deserialize)]
struct UserResponse {
    name: String,
}

impl ManagementClient {
    pub(crate) fn new(base_url: String, username: String, password: String, vhost: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            username,
            password,
            vhost,
        }
    }

    pub(crate) fn from_config(_config: &Config) -> Option<Self> {
        let url = std::env::var("FLUIDBG_RABBITMQ_MANAGEMENT_URL").ok()?;
        Some(Self::new(
            url,
            std::env::var("FLUIDBG_RABBITMQ_MANAGEMENT_USERNAME")
                .unwrap_or_else(|_| "guest".to_string()),
            std::env::var("FLUIDBG_RABBITMQ_MANAGEMENT_PASSWORD")
                .unwrap_or_else(|_| "guest".to_string()),
            std::env::var("FLUIDBG_RABBITMQ_MANAGEMENT_VHOST").unwrap_or_else(|_| "/".to_string()),
        ))
    }

    pub(crate) fn base_url(&self) -> &str {
        &self.base_url
    }

    pub(crate) fn vhost(&self) -> &str {
        &self.vhost
    }

    pub(crate) async fn create_inceptor_user(
        &self,
        username: &str,
        password: &str,
        queue_names: &[String],
    ) -> Result<()> {
        let user_url = format!(
            "{}/api/users/{}",
            self.base_url,
            encode_path_segment(username)
        );
        self.http
            .put(&user_url)
            .basic_auth(&self.username, Some(&self.password))
            .json(&json!({
                "password": password,
                "tags": "monitoring"
            }))
            .send()
            .await
            .with_context(|| format!("failed to create RabbitMQ user '{username}'"))?
            .error_for_status()?;

        let queue_regex = queue_permission_regex(queue_names);
        let permissions_url = format!(
            "{}/api/permissions/{}/{}",
            self.base_url,
            encode_path_segment(&self.vhost),
            encode_path_segment(username)
        );
        self.http
            .put(&permissions_url)
            .basic_auth(&self.username, Some(&self.password))
            .json(&json!({
                "configure": "",
                "write": queue_regex,
                "read": queue_regex
            }))
            .send()
            .await
            .with_context(|| format!("failed to set RabbitMQ permissions for '{username}'"))?
            .error_for_status()?;
        Ok(())
    }

    pub(crate) async fn delete_user(&self, username: &str) -> Result<()> {
        let url = format!(
            "{}/api/users/{}",
            self.base_url,
            encode_path_segment(username)
        );
        let response = self
            .http
            .delete(&url)
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await
            .with_context(|| format!("failed to delete RabbitMQ user '{username}'"))?;
        if response.status().as_u16() == 404 {
            return Ok(());
        }
        response.error_for_status()?;
        Ok(())
    }

    pub(crate) async fn list_users(&self) -> Result<Vec<String>> {
        let url = format!("{}/api/users", self.base_url);
        let users = self
            .http
            .get(&url)
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await
            .context("failed to list RabbitMQ users")?
            .error_for_status()?
            .json::<Vec<UserResponse>>()
            .await
            .context("failed to decode RabbitMQ users")?;
        Ok(users.into_iter().map(|user| user.name).collect())
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

    pub(crate) async fn list_queues(&self) -> Result<Vec<String>> {
        let url = format!(
            "{}/api/queues/{}",
            self.base_url,
            encode_path_segment(&self.vhost)
        );
        let queues = self
            .http
            .get(&url)
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await
            .context("failed to list RabbitMQ queues")?
            .error_for_status()?
            .json::<Vec<QueueListResponse>>()
            .await
            .context("failed to decode RabbitMQ queues")?;
        Ok(queues.into_iter().map(|queue| queue.name).collect())
    }
}

fn queue_permission_regex(queue_names: &[String]) -> String {
    let mut names = queue_names
        .iter()
        .filter(|name| !name.is_empty())
        .map(|name| regex::escape(name))
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    if names.is_empty() {
        "^$".to_string()
    } else {
        format!("^({})$", names.join("|"))
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

    #[test]
    fn queue_permission_regex_is_exact_and_escaped() {
        assert_eq!(
            queue_permission_regex(&["orders".to_string(), "orders.dlq".to_string()]),
            "^(orders|orders\\.dlq)$"
        );
    }
}
