use anyhow::Result;
use async_nats::jetstream::{
    self,
    consumer::{DeliverPolicy, pull},
    stream::{
        Config as StreamConfig, DiscardPolicy, PersistenceMode, Placement, RetentionPolicy,
        StorageType, SubjectTransform,
    },
};
use bytes::Bytes;
use futures::StreamExt;

use crate::config;

#[derive(Clone)]
pub(crate) struct NatsClient {
    client: async_nats::Client,
    jetstream: jetstream::Context,
}

#[derive(Debug)]
pub(crate) struct ConsumerBacklog {
    pub(crate) pending: u64,
    pub(crate) ack_pending: usize,
}

impl NatsClient {
    pub(crate) async fn connect(url: &str) -> Result<Self> {
        let mut options = async_nats::ConnectOptions::new();
        if env_flag("FLUIDBG_NATS_REQUIRE_TLS") {
            options = options.require_tls(true);
        }
        if let Some(path) = optional_env("FLUIDBG_NATS_CA_CERT_PATH") {
            options = options.add_root_certificates(path.into());
        }
        let client = options.connect(url).await?;
        Ok(Self {
            jetstream: jetstream::new(client.clone()),
            client,
        })
    }

    pub(crate) async fn publish_core(&self, subject: &str, payload: Vec<u8>) -> Result<()> {
        self.client
            .publish(subject.to_string(), Bytes::from(payload))
            .await?;
        self.client.flush().await?;
        Ok(())
    }

    pub(crate) async fn subscribe_core(
        &self,
        subject: &str,
        queue_group: Option<&str>,
    ) -> Result<async_nats::Subscriber> {
        let subscriber = if let Some(queue_group) = queue_group.filter(|value| !value.is_empty()) {
            self.client
                .queue_subscribe(subject.to_string(), queue_group.to_string())
                .await?
        } else {
            self.client.subscribe(subject.to_string()).await?
        };
        self.client.flush().await?;
        Ok(subscriber)
    }

    pub(crate) async fn ensure_subject_stream(
        &self,
        subject: &str,
        cfg: &config::StreamConfig,
    ) -> Result<()> {
        self.jetstream
            .get_or_create_stream(stream_config(subject, cfg))
            .await?;
        Ok(())
    }

    pub(crate) async fn delete_subject_stream(&self, subject: &str) -> Result<()> {
        self.delete_stream_name(&stream_name(subject)).await
    }

    pub(crate) async fn delete_stream_name(&self, stream: &str) -> Result<()> {
        let _ = self.jetstream.delete_stream(stream.to_string()).await;
        Ok(())
    }

    pub(crate) async fn publish(&self, subject: &str, payload: Vec<u8>) -> Result<()> {
        self.jetstream
            .publish(subject.to_string(), Bytes::from(payload))
            .await?
            .await?;
        Ok(())
    }

    pub(crate) async fn next_message(
        &self,
        subject: &str,
        durable: &str,
    ) -> Result<Option<jetstream::Message>> {
        self.next_message_inner(subject, durable, None, DeliverPolicy::All)
            .await
    }

    pub(crate) async fn next_message_new(
        &self,
        subject: &str,
        durable: &str,
    ) -> Result<Option<jetstream::Message>> {
        self.next_message_inner(subject, durable, None, DeliverPolicy::New)
            .await
    }

    async fn next_message_for_drain(
        &self,
        subject: &str,
        durable: &str,
    ) -> Result<Option<jetstream::Message>> {
        self.next_message_inner(
            subject,
            durable,
            Some(std::time::Duration::from_millis(250)),
            DeliverPolicy::All,
        )
        .await
    }

    async fn next_message_inner(
        &self,
        subject: &str,
        durable: &str,
        timeout: Option<std::time::Duration>,
        deliver_policy: DeliverPolicy,
    ) -> Result<Option<jetstream::Message>> {
        let stream = self
            .jetstream
            .get_or_create_stream(StreamConfig {
                name: stream_name(subject),
                subjects: vec![subject.to_string()],
                ..Default::default()
            })
            .await?;
        let consumer = stream
            .get_or_create_consumer(
                durable,
                pull::Config {
                    durable_name: Some(durable.to_string()),
                    ack_policy: async_nats::jetstream::consumer::AckPolicy::Explicit,
                    deliver_policy,
                    ..Default::default()
                },
            )
            .await?;
        let mut messages = consumer.fetch().max_messages(1).messages().await?;
        let next = match timeout {
            Some(timeout) => tokio::time::timeout(timeout, messages.next())
                .await
                .unwrap_or_default(),
            None => messages.next().await,
        }
        .transpose()
        .map_err(|err| anyhow::anyhow!(err.to_string()))?;
        Ok(next)
    }

    pub(crate) async fn consumer_backlog(
        &self,
        subject: &str,
        durable: &str,
    ) -> Result<ConsumerBacklog> {
        let stream = self
            .jetstream
            .get_or_create_stream(StreamConfig {
                name: stream_name(subject),
                subjects: vec![subject.to_string()],
                ..Default::default()
            })
            .await?;
        let mut consumer = stream
            .get_or_create_consumer(
                durable,
                pull::Config {
                    durable_name: Some(durable.to_string()),
                    ack_policy: async_nats::jetstream::consumer::AckPolicy::Explicit,
                    ..Default::default()
                },
            )
            .await?;
        let info = consumer.info().await?;
        Ok(ConsumerBacklog {
            pending: info.num_pending,
            ack_pending: info.num_ack_pending,
        })
    }

    pub(crate) async fn move_subject_messages(
        &self,
        source: &str,
        target: &str,
        durable: &str,
        max: usize,
    ) -> Result<u64> {
        let mut moved = 0;
        for _ in 0..max {
            let Some(message) = self.next_message_for_drain(source, durable).await? else {
                break;
            };
            self.publish(target, message.payload.to_vec()).await?;
            message
                .double_ack()
                .await
                .map_err(|err| anyhow::anyhow!(err.to_string()))?;
            moved += 1;
        }
        Ok(moved)
    }

    pub(crate) async fn list_stream_names(&self) -> Result<Vec<String>> {
        let mut names = self.jetstream.stream_names();
        let mut result = Vec::new();
        while let Some(name) = names.next().await {
            result.push(name?);
        }
        Ok(result)
    }
}

fn optional_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}

fn stream_config(subject: &str, cfg: &config::StreamConfig) -> StreamConfig {
    let mut subjects = if cfg.subjects.is_empty() {
        vec![subject.to_string()]
    } else {
        cfg.subjects.clone()
    };
    if !subjects.iter().any(|configured| configured == subject) {
        subjects.push(subject.to_string());
    }
    let storage = match cfg.storage.as_deref() {
        Some("memory") => StorageType::Memory,
        _ => StorageType::File,
    };
    let retention = match cfg.retention.as_deref() {
        Some("interest") => RetentionPolicy::Interest,
        Some("workqueue") | Some("workQueue") => RetentionPolicy::WorkQueue,
        _ => RetentionPolicy::Limits,
    };
    let discard = match cfg.discard.as_deref() {
        Some("new") => DiscardPolicy::New,
        _ => DiscardPolicy::Old,
    };
    let persist_mode = match cfg.persistence_mode.as_deref() {
        Some("async") => Some(PersistenceMode::Async),
        Some("default") => Some(PersistenceMode::Default),
        _ => None,
    };
    StreamConfig {
        name: stream_name(subject),
        subjects,
        storage,
        retention,
        discard,
        num_replicas: cfg.replicas.unwrap_or(1),
        max_messages: cfg.max_messages.unwrap_or(-1),
        max_bytes: cfg.max_bytes.unwrap_or(-1),
        max_age: std::time::Duration::from_secs(cfg.max_age_seconds.unwrap_or(0).max(0) as u64),
        allow_rollup: cfg.allow_rollup.unwrap_or(false),
        deny_delete: cfg.deny_delete.unwrap_or(false),
        deny_purge: cfg.deny_purge.unwrap_or(false),
        placement: cfg.placement.as_ref().map(|placement| Placement {
            cluster: placement.cluster.clone(),
            tags: placement.tags.clone(),
        }),
        persist_mode,
        subject_transform: cfg
            .subject_transform
            .as_ref()
            .map(|transform| SubjectTransform {
                source: transform.source.clone(),
                destination: transform.destination.clone(),
            }),
        metadata: cfg.metadata.clone().into_iter().collect(),
        ..Default::default()
    }
}

pub(crate) fn stream_name(subject: &str) -> String {
    let mut hash = 5381u64;
    for byte in subject.as_bytes() {
        hash = hash.wrapping_mul(33).wrapping_add(u64::from(*byte));
    }
    let hint = subject
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(24)
        .collect::<String>();
    format!("fbg_{hint}_{hash:016x}")
}

pub(crate) fn durable_name(prefix: &str, subject: &str) -> String {
    stream_name(&format!("{prefix}_{subject}"))
}

pub(crate) fn queue_group_durable(queue_group: &str, subject: &str) -> String {
    stream_name(&format!("{queue_group}_{subject}"))
}

#[cfg(test)]
mod tests {
    use super::{stream_config, stream_name};

    #[test]
    fn stream_names_are_bounded_and_stable() {
        let name = stream_name("fluidbg-green-input-really-long-demo-subject");
        assert!(name.starts_with("fbg_"));
        assert!(name.contains("fluidbg-green-input"));
        assert!(name.len() <= 64);
        assert_eq!(
            name,
            stream_name("fluidbg-green-input-really-long-demo-subject")
        );
    }

    #[test]
    fn stream_config_preserves_user_subjects_and_persistence_options() {
        let cfg: crate::config::StreamConfig = serde_json::from_value(serde_json::json!({
            "subjects": ["orders.a.*"],
            "storage": "file",
            "retention": "limits",
            "replicas": 3,
            "persistenceMode": "async",
            "subjectTransform": {"source": "orders.a.*", "destination": "orders.partitioned.$1"},
            "metadata": {"partition": "a"}
        }))
        .unwrap();
        let rendered = stream_config("orders", &cfg);
        assert_eq!(rendered.name, stream_name("orders"));
        assert!(rendered.subjects.contains(&"orders.a.*".to_string()));
        assert!(rendered.subjects.contains(&"orders".to_string()));
        assert_eq!(rendered.num_replicas, 3);
        assert!(rendered.persist_mode.is_some());
        assert!(rendered.subject_transform.is_some());
        assert_eq!(
            rendered.metadata.get("partition").map(String::as_str),
            Some("a")
        );
    }
}
