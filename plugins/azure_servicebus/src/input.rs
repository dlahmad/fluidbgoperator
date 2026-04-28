use std::time::Duration;

use anyhow::Result;
use fluidbg_plugin_sdk::{PluginRole, TrafficRoute};
use serde_json::Value;
use tracing::{info, warn};

use crate::config::{
    AppState, RuntimeMode, consumer_config, duplicator_config, has_role, inceptor_infra_disabled,
    observer_config, required, routes_to_blue, shadow_queue_name, splitter_config,
};
use crate::filtering::{extract_test_id, matches_filter, notify_observer};
use crate::servicebus::LockedMessage;

async fn process_input_message(state: &AppState, message: &LockedMessage) -> Result<TrafficRoute> {
    let body_json: Value = serde_json::from_slice(&message.body).unwrap_or(Value::Null);
    let observer = observer_config(&state.config);
    let observer_matches =
        observer.is_some_and(|o| matches_filter(&o.r#match, &body_json, &message.properties));

    let mut route = TrafficRoute::Unknown;

    if has_role(&state.roles, PluginRole::Duplicator) {
        let duplicator = duplicator_config(&state.config)?;
        if let Some(green_queue) = &duplicator.green_input_queue {
            state
                .service_bus
                .forward_message(green_queue, message)
                .await?;
        }
        if let Some(blue_queue) = &duplicator.blue_input_queue {
            state
                .service_bus
                .forward_message(blue_queue, message)
                .await?;
        }
        route = TrafficRoute::Both;
    } else if has_role(&state.roles, PluginRole::Splitter) {
        let splitter = splitter_config(&state.config)?;
        if routes_to_blue(&message.body, state.traffic_percent()) {
            if let Some(blue_queue) = &splitter.blue_input_queue {
                state
                    .service_bus
                    .forward_message(blue_queue, message)
                    .await?;
            }
            route = TrafficRoute::Blue;
        } else if let Some(green_queue) = &splitter.green_input_queue {
            state
                .service_bus
                .forward_message(green_queue, message)
                .await?;
            route = TrafficRoute::Green;
        }
    }

    if has_role(&state.roles, PluginRole::Observer)
        && observer_matches
        && let Some(observer) = observer
        && let Some(selector) = &observer.test_id
        && let Some(test_id) = extract_test_id(selector, &body_json, &message.properties)
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

    Ok(route)
}

pub(crate) async fn drain_input_queues(state: &AppState) -> Result<()> {
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

    if !inceptor_infra_disabled() {
        state
            .service_bus
            .create_queue(&base_queue, &state.config.queue_declaration)
            .await?;
        if let Some(base_shadow_queue) = shadow_queue_name(&state.config, &base_queue) {
            let shadow_declaration = state
                .config
                .shadow_queue
                .as_ref()
                .map(|shadow| &shadow.queue_declaration)
                .unwrap_or(&state.config.queue_declaration);
            state
                .service_bus
                .create_queue(&base_shadow_queue, shadow_declaration)
                .await?;
        }
    }
    for source in [green_queue, blue_queue] {
        let moved = state
            .service_bus
            .move_available_messages(&source, &base_queue)
            .await?;
        if moved > 0 {
            info!(
                "moved {} Service Bus message(s) from {} back to {} during drain",
                moved, source, base_queue
            );
        }
        let moved_dead_letter = state
            .service_bus
            .move_available_dead_letter_messages(&source, &base_queue)
            .await?;
        if moved_dead_letter > 0 {
            info!(
                "moved {} Service Bus dead-letter message(s) from {} back to {} during drain",
                moved_dead_letter, source, base_queue
            );
        }
        if let Some(shadow_queue) = shadow_queue_name(&state.config, &source) {
            let target_shadow_queue =
                shadow_queue_name(&state.config, &base_queue).unwrap_or_else(|| base_queue.clone());
            let moved = state
                .service_bus
                .move_available_messages(&shadow_queue, &target_shadow_queue)
                .await?;
            if moved > 0 {
                info!(
                    "moved {} Service Bus shadow message(s) from {} back to {} during drain",
                    moved, shadow_queue, target_shadow_queue
                );
            }
            let moved_dead_letter = state
                .service_bus
                .move_available_dead_letter_messages(&shadow_queue, &target_shadow_queue)
                .await?;
            if moved_dead_letter > 0 {
                info!(
                    "moved {} Service Bus shadow dead-letter message(s) from {} back to {} during drain",
                    moved_dead_letter, shadow_queue, target_shadow_queue
                );
            }
        }
    }

    Ok(())
}

pub(crate) async fn run_input_pipeline(state: AppState) -> Result<()> {
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

    loop {
        match state.runtime_mode() {
            RuntimeMode::Idle => {
                tokio::time::sleep(Duration::from_millis(300)).await;
                continue;
            }
            RuntimeMode::Draining => {
                if let Err(err) = drain_input_queues(&state).await {
                    warn!("azure service bus input drain failed: {}", err);
                }
                tokio::time::sleep(Duration::from_millis(300)).await;
                continue;
            }
            RuntimeMode::Active => {}
        }

        let message = match state.service_bus.receive_peek_lock(&input_queue, 1).await {
            Ok(Some(message)) => message,
            Ok(None) => {
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            }
            Err(err) => {
                warn!("azure service bus input poll failed: {}", err);
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        let _in_flight = state.track_message();
        if !matches!(state.runtime_mode(), RuntimeMode::Active) {
            if let Err(err) = state.service_bus.abandon(&message).await {
                warn!(
                    "azure service bus input unlock after drain started failed: {}",
                    err
                );
            }
            continue;
        }

        match process_input_message(&state, &message).await {
            Ok(_) => {
                if let Err(err) = state.service_bus.complete(&message).await {
                    warn!("azure service bus input complete failed: {}", err);
                }
            }
            Err(err) => {
                warn!("azure service bus input processing failed: {}", err);
                if let Err(abandon_err) = state.service_bus.abandon(&message).await {
                    warn!(
                        "azure service bus input unlock after failure failed: {}",
                        abandon_err
                    );
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}
