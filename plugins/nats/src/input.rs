use std::time::Duration;

use anyhow::Result;
use fluidbg_plugin_sdk::{PluginRole, TrafficRoute};
use serde_json::Value;
use tracing::{debug, info, warn};

use crate::config::{
    AppState, Config, RuntimeMode, consumer_config, duplicator_config, has_role, required,
    routes_to_blue, splitter_config,
};
use crate::filtering::{extract_test_id, matches_filter, notify_observer};
use crate::nats::{NatsClient, durable_name, queue_group_durable};

async fn process_input_message(
    state: &AppState,
    client: &NatsClient,
    message: async_nats::jetstream::Message,
) -> Result<()> {
    let body = message.payload.to_vec();
    let body_json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let mut route = TrafficRoute::Unknown;

    if has_role(&state.roles, PluginRole::Duplicator) {
        let config = duplicator_config(&state.config)?;
        if let Some(subject) = &config.green_input_subject {
            client.publish(subject, body.clone()).await?;
        }
        if let Some(subject) = &config.blue_input_subject {
            client.publish(subject, body.clone()).await?;
        }
        route = TrafficRoute::Both;
    } else if has_role(&state.roles, PluginRole::Splitter) {
        let config = splitter_config(&state.config)?;
        if routes_to_blue(&body, state.traffic_percent()) {
            if let Some(subject) = &config.blue_input_subject {
                client.publish(subject, body.clone()).await?;
            }
            route = TrafficRoute::Blue;
        } else if let Some(subject) = &config.green_input_subject {
            client.publish(subject, body.clone()).await?;
            route = TrafficRoute::Green;
        }
    }

    if has_role(&state.roles, PluginRole::Observer)
        && let Some(observer) = &state.config.observer
        && matches_filter(&observer.r#match, &body_json)
        && let Some(selector) = &observer.test_id
        && let Some(test_id) = extract_test_id(selector, &body_json)
    {
        let notified = notify_observer(state, observer, &test_id, &body_json, route).await;
        if notified && route.should_register_case() {
            state.runtime.register_test_case(&test_id).await?;
        }
    }

    message
        .double_ack()
        .await
        .map_err(|err| anyhow::anyhow!(err.to_string()))?;
    Ok(())
}

pub(crate) async fn drain_input_subjects(state: &AppState) -> Result<()> {
    if !has_role(&state.roles, PluginRole::Duplicator)
        && !has_role(&state.roles, PluginRole::Splitter)
    {
        return Ok(());
    }
    let (base, green, green_durable, blue, blue_durable) = input_drain_targets(state)?;
    let client = NatsClient::connect(&state.nats_url).await?;
    for (source, durable) in [(green, green_durable), (blue, blue_durable)] {
        let moved = client
            .move_subject_messages(source, base, &durable, 10_000)
            .await?;
        if moved > 0 {
            info!(
                "moved {} NATS message(s) from {} back to {}",
                moved, source, base
            );
        }
    }
    Ok(())
}

pub(crate) fn input_drain_targets(state: &AppState) -> Result<(&str, &str, String, &str, String)> {
    if has_role(&state.roles, PluginRole::Duplicator) {
        let config = duplicator_config(&state.config)?;
        Ok((
            required(&config.input_subject, "duplicator.inputSubject")?,
            required(&config.green_input_subject, "duplicator.greenInputSubject")?,
            input_consumer_durable(
                config
                    .green_queue_group
                    .as_deref()
                    .or(config.queue_group.as_deref()),
                required(&config.green_input_subject, "duplicator.greenInputSubject")?,
            ),
            required(&config.blue_input_subject, "duplicator.blueInputSubject")?,
            input_consumer_durable(
                config
                    .blue_queue_group
                    .as_deref()
                    .or(config.queue_group.as_deref()),
                required(&config.blue_input_subject, "duplicator.blueInputSubject")?,
            ),
        ))
    } else {
        let config = splitter_config(&state.config)?;
        Ok((
            required(&config.input_subject, "splitter.inputSubject")?,
            required(&config.green_input_subject, "splitter.greenInputSubject")?,
            input_consumer_durable(
                config
                    .green_queue_group
                    .as_deref()
                    .or(config.queue_group.as_deref()),
                required(&config.green_input_subject, "splitter.greenInputSubject")?,
            ),
            required(&config.blue_input_subject, "splitter.blueInputSubject")?,
            input_consumer_durable(
                config
                    .blue_queue_group
                    .as_deref()
                    .or(config.queue_group.as_deref()),
                required(&config.blue_input_subject, "splitter.blueInputSubject")?,
            ),
        ))
    }
}

fn input_consumer_durable(queue_group: Option<&str>, subject: &str) -> String {
    queue_group
        .map(|group| queue_group_durable(group, subject))
        .unwrap_or_else(|| durable_name("drain", subject))
}

pub(crate) async fn run_input_pipeline(state: AppState) -> Result<()> {
    loop {
        if matches!(state.runtime_mode(), RuntimeMode::Idle) {
            tokio::time::sleep(Duration::from_millis(300)).await;
            continue;
        }
        let client = match NatsClient::connect(&state.nats_url).await {
            Ok(client) => client,
            Err(err) => {
                warn!("nats input pipeline connect failed, reconnecting: {}", err);
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        let input_subject = if has_role(&state.roles, PluginRole::Duplicator) {
            required(
                &duplicator_config(&state.config)?.input_subject,
                "duplicator.inputSubject",
            )?
        } else if has_role(&state.roles, PluginRole::Splitter) {
            required(
                &splitter_config(&state.config)?.input_subject,
                "splitter.inputSubject",
            )?
        } else {
            required(
                &consumer_config(&state.config)?.input_subject,
                "consumer.inputSubject",
            )?
        }
        .to_string();
        debug!("nats input pipeline polling {}", input_subject);

        loop {
            match state.runtime_mode() {
                RuntimeMode::Idle => {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    continue;
                }
                RuntimeMode::Draining => {
                    if let Err(err) = drain_input_subjects(&state).await {
                        debug!("nats input drain failed: {}", err);
                    }
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    continue;
                }
                RuntimeMode::Active => {}
            }

            let source_durable = input_source_durable(&state.config, &state.roles, &input_subject)?;
            let next = if has_role(&state.roles, PluginRole::Duplicator)
                || has_role(&state.roles, PluginRole::Splitter)
            {
                if source_durable.uses_existing_consumer {
                    client
                        .next_message(&input_subject, &source_durable.name)
                        .await
                } else {
                    client
                        .next_message_new(&input_subject, &source_durable.name)
                        .await
                }
            } else {
                client
                    .next_message(&input_subject, &source_durable.name)
                    .await
            };

            match next {
                Ok(Some(message)) => {
                    if let Err(err) = process_input_message(&state, &client, message).await {
                        warn!("nats input processing failed, reconnecting: {}", err);
                        break;
                    }
                }
                Ok(None) => tokio::time::sleep(Duration::from_millis(200)).await,
                Err(err) => {
                    warn!("nats input poll failed, reconnecting: {}", err);
                    break;
                }
            }
        }
    }
}

struct SourceDurable {
    name: String,
    uses_existing_consumer: bool,
}

fn input_source_durable(
    config: &Config,
    roles: &[PluginRole],
    input_subject: &str,
) -> Result<SourceDurable> {
    if has_role(roles, PluginRole::Duplicator) {
        let config = duplicator_config(config)?;
        if let Some(queue_group) = config.queue_group.as_deref() {
            return Ok(SourceDurable {
                name: queue_group_durable(queue_group, input_subject),
                uses_existing_consumer: true,
            });
        }
    }
    if has_role(roles, PluginRole::Splitter) {
        let config = splitter_config(config)?;
        if let Some(queue_group) = config.queue_group.as_deref() {
            return Ok(SourceDurable {
                name: queue_group_durable(queue_group, input_subject),
                uses_existing_consumer: true,
            });
        }
    }
    Ok(SourceDurable {
        name: durable_name("input", input_subject),
        uses_existing_consumer: false,
    })
}
