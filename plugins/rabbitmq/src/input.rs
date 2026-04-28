use std::time::Duration;

use anyhow::Result;
use lapin::Channel;
use lapin::options::{BasicAckOptions, BasicGetOptions};
use serde_json::Value;
use tracing::{info, warn};

use crate::amqp::{connect_with_retry, declare_queue, move_queue_messages, publish_confirmed};
use crate::config::{
    AppState, RuntimeMode, consumer_config, duplicator_config, has_role, inceptor_infra_disabled,
    observer_config, required, routes_to_blue, shadow_queue_name, splitter_config,
};
use crate::filtering::{extract_test_id, matches_filter, notify_observer};
use fluidbg_plugin_sdk::{PluginRole, TrafficRoute};

async fn process_input_delivery(
    state: &AppState,
    channel: &Channel,
    delivery: lapin::message::BasicGetMessage,
) -> Result<()> {
    let body_data = delivery.data.clone();
    let body_str = String::from_utf8_lossy(&body_data);
    let body_json: Value = serde_json::from_str(&body_str).unwrap_or(Value::Null);
    let headers = delivery.properties.headers().clone().unwrap_or_default();

    let observer = observer_config(&state.config);
    let observer_matches =
        observer.is_some_and(|o| matches_filter(&o.r#match, &body_json, &headers));

    let mut route = TrafficRoute::Unknown;

    if has_role(&state.roles, PluginRole::Duplicator) {
        let duplicator = duplicator_config(&state.config)?;
        if let Some(green_queue) = &duplicator.green_input_queue {
            publish_confirmed(
                channel,
                green_queue,
                &body_data,
                delivery.properties.clone(),
            )
            .await?;
        }
        if let Some(blue_queue) = &duplicator.blue_input_queue {
            publish_confirmed(channel, blue_queue, &body_data, delivery.properties.clone()).await?;
        }
        route = TrafficRoute::Both;
    } else if has_role(&state.roles, PluginRole::Splitter) {
        let splitter = splitter_config(&state.config)?;
        let send_to_blue = routes_to_blue(&body_data, state.traffic_percent());
        if send_to_blue {
            if let Some(blue_queue) = &splitter.blue_input_queue {
                publish_confirmed(channel, blue_queue, &body_data, delivery.properties.clone())
                    .await?;
            }
            route = TrafficRoute::Blue;
        } else if let Some(green_queue) = &splitter.green_input_queue {
            publish_confirmed(
                channel,
                green_queue,
                &body_data,
                delivery.properties.clone(),
            )
            .await?;
            route = TrafficRoute::Green;
        }
    }

    if has_role(&state.roles, PluginRole::Observer)
        && observer_matches
        && let Some(observer) = observer
        && let Some(selector) = &observer.test_id
        && let Some(test_id) = extract_test_id(selector, &body_json, &headers)
    {
        let notified = notify_observer(state, observer, &test_id, &body_json, route).await;
        if notified {
            if route.should_register_case()
                && let Err(err) = state.runtime.register_test_case(&test_id).await
            {
                warn!("failed to register test case {}: {}", test_id, err);
            } else if route.should_register_case() {
                info!(
                    "registered testCase '{}' for blueGreenRef '{}'",
                    test_id,
                    state.runtime.blue_green_ref()
                );
            }
        }
    }

    delivery.ack(BasicAckOptions::default()).await?;
    Ok(())
}

async fn drain_input_queues(state: &AppState, channel: &Channel) -> Result<()> {
    let (base_queue, green_queue, blue_queue) = if has_role(&state.roles, PluginRole::Duplicator) {
        let config = duplicator_config(&state.config)?;
        (
            required(&config.input_queue, "duplicator.inputQueue")?.to_string(),
            required(&config.green_input_queue, "duplicator.greenInputQueue")?.to_string(),
            required(&config.blue_input_queue, "duplicator.blueInputQueue")?.to_string(),
        )
    } else if has_role(&state.roles, PluginRole::Splitter) {
        let config = splitter_config(&state.config)?;
        (
            required(&config.input_queue, "splitter.inputQueue")?.to_string(),
            required(&config.green_input_queue, "splitter.greenInputQueue")?.to_string(),
            required(&config.blue_input_queue, "splitter.blueInputQueue")?.to_string(),
        )
    } else {
        return Ok(());
    };

    if !inceptor_infra_disabled()
        && let Some(base_shadow_queue) = shadow_queue_name(&state.config, &base_queue)
    {
        let shadow_declaration = state
            .config
            .shadow_queue
            .as_ref()
            .map(|shadow| &shadow.queue_declaration)
            .unwrap_or(&state.config.queue_declaration);
        declare_queue(channel, &base_shadow_queue, shadow_declaration).await?;
    }

    for source in [&green_queue, &blue_queue] {
        let moved = move_queue_messages(channel, source, &base_queue).await?;
        if moved > 0 {
            info!(
                "moved {} message(s) from {} back to {} during drain",
                moved, source, base_queue
            );
        }
        if let Some(shadow_queue) = shadow_queue_name(&state.config, source) {
            let target_shadow_queue =
                shadow_queue_name(&state.config, &base_queue).unwrap_or_else(|| base_queue.clone());
            let moved = move_queue_messages(channel, &shadow_queue, &target_shadow_queue).await?;
            if moved > 0 {
                info!(
                    "moved {} shadow message(s) from {} back to {} during drain",
                    moved, shadow_queue, target_shadow_queue
                );
            }
        }
    }

    Ok(())
}

pub(crate) async fn drain_input_roles(state: &AppState) -> Result<()> {
    if !has_role(&state.roles, PluginRole::Duplicator)
        && !has_role(&state.roles, PluginRole::Splitter)
    {
        return Ok(());
    }

    let conn = connect_with_retry(&state.amqp_url).await?;
    let channel = conn.create_channel().await?;
    drain_input_queues(state, &channel).await
}

pub(crate) async fn run_input_pipeline(state: AppState) -> Result<()> {
    loop {
        if matches!(state.runtime_mode(), RuntimeMode::Idle) {
            tokio::time::sleep(Duration::from_millis(300)).await;
            continue;
        }
        let conn = match connect_with_retry(&state.amqp_url).await {
            Ok(conn) => conn,
            Err(err) => {
                warn!(
                    "rabbitmq input pipeline connect failed, reconnecting: {}",
                    err
                );
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        let channel = match conn.create_channel().await {
            Ok(channel) => channel,
            Err(err) => {
                warn!(
                    "rabbitmq input pipeline channel failed, reconnecting: {}",
                    err
                );
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        let input_queue = if has_role(&state.roles, PluginRole::Duplicator) {
            required(
                &duplicator_config(&state.config)?.input_queue,
                "duplicator.inputQueue",
            )?
            .to_string()
        } else if has_role(&state.roles, PluginRole::Splitter) {
            required(
                &splitter_config(&state.config)?.input_queue,
                "splitter.inputQueue",
            )?
            .to_string()
        } else {
            required(
                &consumer_config(&state.config)?.input_queue,
                "consumer.inputQueue",
            )?
            .to_string()
        };
        if !inceptor_infra_disabled() {
            declare_queue(&channel, &input_queue, &state.config.queue_declaration).await?;
        }
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
                .basic_get(input_queue.as_str().into(), BasicGetOptions::default())
                .await
            {
                Ok(Some(delivery)) => {
                    if let Err(err) = process_input_delivery(&state, &channel, delivery).await {
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
