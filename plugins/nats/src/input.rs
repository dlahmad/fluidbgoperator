use std::time::Duration;

use anyhow::Result;
use fluidbg_plugin_sdk::{PluginRole, TrafficRoute};
use serde_json::Value;
use tracing::{debug, info, warn};

use crate::config::{
    AppState, RuntimeMode, consumer_config, duplicator_config, has_role, required, routes_to_blue,
    splitter_config,
};
use crate::filtering::{extract_test_id, matches_filter, notify_observer};
use crate::nats::{NatsClient, durable_name};

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
        .ack()
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
    let (base, green, blue) = if has_role(&state.roles, PluginRole::Duplicator) {
        let config = duplicator_config(&state.config)?;
        (
            required(&config.input_subject, "duplicator.inputSubject")?,
            required(&config.green_input_subject, "duplicator.greenInputSubject")?,
            required(&config.blue_input_subject, "duplicator.blueInputSubject")?,
        )
    } else {
        let config = splitter_config(&state.config)?;
        (
            required(&config.input_subject, "splitter.inputSubject")?,
            required(&config.green_input_subject, "splitter.greenInputSubject")?,
            required(&config.blue_input_subject, "splitter.blueInputSubject")?,
        )
    };
    let client = NatsClient::connect(&state.nats_url).await?;
    for source in [green, blue] {
        let moved = client
            .move_subject_messages(source, base, &durable_name("drain", source), 10_000)
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

            match client
                .next_message(&input_subject, &durable_name("input", &input_subject))
                .await
            {
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
