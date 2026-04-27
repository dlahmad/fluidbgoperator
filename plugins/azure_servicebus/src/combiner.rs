use std::time::Duration;

use anyhow::Result;
use fluidbg_plugin_sdk::PluginRole;
use serde_json::Value;
use tracing::{info, warn};

use crate::config::{
    AppState, RuntimeMode, combiner_config, has_role, inceptor_infra_disabled, observer_config,
    required, shadow_queue_name,
};
use crate::filtering::{
    extract_test_id, matches_filter, notify_observer, route_from_output_source,
};

async fn run_combine_loop(
    source_queue: String,
    result_queue: String,
    state: AppState,
) -> Result<()> {
    let combiner = combiner_config(&state.config)?.clone();

    loop {
        match state.runtime_mode() {
            RuntimeMode::Idle => {
                tokio::time::sleep(Duration::from_millis(300)).await;
                continue;
            }
            RuntimeMode::Draining => {
                if let Err(err) = drain_output_queue(&state, &source_queue, &result_queue).await {
                    warn!(
                        "azure service bus combiner drain from '{}' failed: {}",
                        source_queue, err
                    );
                }
                tokio::time::sleep(Duration::from_millis(300)).await;
                continue;
            }
            RuntimeMode::Active => {}
        }

        let message = match state.service_bus.receive_peek_lock(&source_queue, 1).await {
            Ok(Some(message)) => message,
            Ok(None) => {
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            }
            Err(err) => {
                warn!(
                    "azure service bus combiner poll from '{}' failed: {}",
                    source_queue, err
                );
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        let _in_flight = state.track_message();
        if !matches!(state.runtime_mode(), RuntimeMode::Active) {
            if let Err(err) = state.service_bus.abandon(&message).await {
                warn!(
                    "azure service bus combiner unlock after drain started from '{}' failed: {}",
                    source_queue, err
                );
            }
            continue;
        }

        let route = route_from_output_source(&combiner, &source_queue);
        let body_json: Value = serde_json::from_slice(&message.body).unwrap_or(Value::Null);
        let result = async {
            state
                .service_bus
                .send_message(&result_queue, &message.body, &message.properties)
                .await?;

            if has_role(&state.roles, PluginRole::Observer)
                && let Some(observer) = observer_config(&state.config)
                && matches_filter(&observer.r#match, &body_json, &message.properties)
                && let Some(selector) = &observer.test_id
                && let Some(test_id) = extract_test_id(selector, &body_json, &message.properties)
            {
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
                notify_observer(&state, observer, &test_id, &body_json, route).await;
            }

            Ok::<(), anyhow::Error>(())
        }
        .await;

        match result {
            Ok(()) => {
                if let Err(err) = state.service_bus.complete(&message).await {
                    warn!(
                        "azure service bus combiner complete from '{}' failed: {}",
                        source_queue, err
                    );
                }
            }
            Err(err) => {
                warn!(
                    "azure service bus combiner processing from '{}' failed: {}",
                    source_queue, err
                );
                if let Err(abandon_err) = state.service_bus.abandon(&message).await {
                    warn!(
                        "azure service bus combiner unlock after failure failed: {}",
                        abandon_err
                    );
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

async fn drain_output_queue(
    state: &AppState,
    source_queue: &str,
    result_queue: &str,
) -> Result<()> {
    if !inceptor_infra_disabled() {
        state
            .service_bus
            .create_queue(result_queue, &state.config.queue_declaration)
            .await?;
        if let Some(result_shadow_queue) = shadow_queue_name(&state.config, result_queue) {
            let shadow_declaration = state
                .config
                .shadow_queue
                .as_ref()
                .map(|shadow| &shadow.queue_declaration)
                .unwrap_or(&state.config.queue_declaration);
            state
                .service_bus
                .create_queue(&result_shadow_queue, shadow_declaration)
                .await?;
        }
    }
    let moved = state
        .service_bus
        .move_available_messages(source_queue, result_queue)
        .await?;
    if moved > 0 {
        info!(
            "moved {} Service Bus output message(s) from {} back to {} during drain",
            moved, source_queue, result_queue
        );
    }
    let moved_dead_letter = state
        .service_bus
        .move_available_dead_letter_messages(source_queue, result_queue)
        .await?;
    if moved_dead_letter > 0 {
        info!(
            "moved {} Service Bus output dead-letter message(s) from {} back to {} during drain",
            moved_dead_letter, source_queue, result_queue
        );
    }
    if let Some(shadow_queue) = shadow_queue_name(&state.config, source_queue) {
        let target_shadow_queue = shadow_queue_name(&state.config, result_queue)
            .unwrap_or_else(|| result_queue.to_string());
        let moved = state
            .service_bus
            .move_available_messages(&shadow_queue, &target_shadow_queue)
            .await?;
        if moved > 0 {
            info!(
                "moved {} Service Bus output shadow message(s) from {} back to {} during drain",
                moved, shadow_queue, target_shadow_queue
            );
        }
        let moved_dead_letter = state
            .service_bus
            .move_available_dead_letter_messages(&shadow_queue, &target_shadow_queue)
            .await?;
        if moved_dead_letter > 0 {
            info!(
                "moved {} Service Bus output shadow dead-letter message(s) from {} back to {} during drain",
                moved_dead_letter, shadow_queue, target_shadow_queue
            );
        }
    }
    Ok(())
}

pub(crate) async fn drain_output_queues(state: &AppState) -> Result<()> {
    if !has_role(&state.roles, PluginRole::Combiner) {
        return Ok(());
    }

    let config = combiner_config(&state.config)?;
    let green_queue = required(&config.green_output_queue, "combiner.greenOutputQueue")?;
    let blue_queue = required(&config.blue_output_queue, "combiner.blueOutputQueue")?;
    let result_queue = required(&config.output_queue, "combiner.outputQueue")?;

    drain_output_queue(state, green_queue, result_queue).await?;
    drain_output_queue(state, blue_queue, result_queue).await?;
    Ok(())
}

pub(crate) async fn run_combiner(state: AppState) -> Result<()> {
    let config = state.config.clone();
    let combiner = combiner_config(&config)?;
    let blue_queue = required(&combiner.blue_output_queue, "combiner.blueOutputQueue")?.to_string();
    let green_queue =
        required(&combiner.green_output_queue, "combiner.greenOutputQueue")?.to_string();
    let result_queue = required(&combiner.output_queue, "combiner.outputQueue")?.to_string();

    let blue_task = tokio::spawn(run_combine_loop(
        blue_queue,
        result_queue.clone(),
        state.clone(),
    ));
    let green_task = tokio::spawn(run_combine_loop(green_queue, result_queue, state));
    let (blue_result, green_result) = tokio::join!(blue_task, green_task);
    blue_result??;
    green_result??;
    Ok(())
}
