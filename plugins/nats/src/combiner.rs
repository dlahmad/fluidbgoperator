use std::time::Duration;

use anyhow::Result;
use fluidbg_plugin_sdk::PluginRole;
use serde_json::Value;
use tracing::{debug, info, warn};

use crate::config::{AppState, RuntimeMode, combiner_config, has_role, required};
use crate::filtering::{
    extract_test_id, matches_filter, notify_observer, route_from_output_source,
};
use crate::nats::{NatsClient, durable_name};

async fn run_combine_loop(source: String, target: String, state: AppState) -> Result<()> {
    loop {
        if matches!(state.runtime_mode(), RuntimeMode::Idle) {
            tokio::time::sleep(Duration::from_millis(300)).await;
            continue;
        }
        let client = match NatsClient::connect(&state.nats_url).await {
            Ok(client) => client,
            Err(err) => {
                warn!("nats combiner connect failed, reconnecting: {}", err);
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        if matches!(state.runtime_mode(), RuntimeMode::Draining) {
            drain_output_subject(&state, &source, &target).await?;
            tokio::time::sleep(Duration::from_millis(300)).await;
            continue;
        }
        match client
            .next_message(&source, &durable_name("combiner", &source))
            .await
        {
            Ok(Some(message)) => {
                let body = message.payload.to_vec();
                let body_json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                let route = route_from_output_source(combiner_config(&state.config)?, &source);
                client.publish(&target, body).await?;
                if has_role(&state.roles, PluginRole::Observer)
                    && let Some(observer) = &state.config.observer
                    && matches_filter(&observer.r#match, &body_json)
                    && let Some(selector) = &observer.test_id
                    && let Some(test_id) = extract_test_id(selector, &body_json)
                {
                    let notified =
                        notify_observer(&state, observer, &test_id, &body_json, route).await;
                    if notified && route.should_register_case() {
                        state.runtime.register_test_case(&test_id).await?;
                    }
                }
                message
                    .double_ack()
                    .await
                    .map_err(|err| anyhow::anyhow!(err.to_string()))?;
            }
            Ok(None) => tokio::time::sleep(Duration::from_millis(200)).await,
            Err(err) => {
                debug!("nats combiner poll failed, reconnecting: {}", err);
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
}

async fn drain_output_subject(state: &AppState, source: &str, target: &str) -> Result<()> {
    let client = NatsClient::connect(&state.nats_url).await?;
    let moved = client
        .move_subject_messages(source, target, &combiner_durable(source), 10_000)
        .await?;
    if moved > 0 {
        info!(
            "moved {} NATS output message(s) from {} back to {}",
            moved, source, target
        );
    }
    Ok(())
}

pub(crate) fn combiner_durable(source: &str) -> String {
    durable_name("combiner", source)
}

pub(crate) async fn drain_output_subjects(state: &AppState) -> Result<()> {
    if !has_role(&state.roles, PluginRole::Combiner) {
        return Ok(());
    }
    let config = combiner_config(&state.config)?;
    let green = required(&config.green_output_subject, "combiner.greenOutputSubject")?;
    let blue = required(&config.blue_output_subject, "combiner.blueOutputSubject")?;
    let output = required(&config.output_subject, "combiner.outputSubject")?;
    drain_output_subject(state, green, output).await?;
    drain_output_subject(state, blue, output).await?;
    Ok(())
}

pub(crate) async fn run_combiner(state: AppState) -> Result<()> {
    let config = combiner_config(&state.config)?;
    let green = required(&config.green_output_subject, "combiner.greenOutputSubject")?.to_string();
    let blue = required(&config.blue_output_subject, "combiner.blueOutputSubject")?.to_string();
    let output = required(&config.output_subject, "combiner.outputSubject")?.to_string();
    let green_task = tokio::spawn(run_combine_loop(green, output.clone(), state.clone()));
    let blue_task = tokio::spawn(run_combine_loop(blue, output, state));
    green_task.await??;
    blue_task.await??;
    Ok(())
}
