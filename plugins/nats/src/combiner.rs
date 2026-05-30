use std::time::Duration;

use anyhow::Result;
use fluidbg_plugin_sdk::PluginRole;
use futures::StreamExt;
use serde_json::Value;
use tracing::{debug, info, warn};

use crate::config::{
    AppState, CORE_READY_BLUE_OUTPUT, CORE_READY_GREEN_OUTPUT, RuntimeMode, combiner_config,
    has_role, required,
};
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
                publish_to_target(&state, &client, &target, body).await?;
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

async fn run_core_combine_loop(
    source: String,
    target: String,
    state: AppState,
    ready_mask: u8,
) -> Result<()> {
    loop {
        if matches!(
            state.runtime_mode(),
            RuntimeMode::Idle | RuntimeMode::Draining
        ) {
            tokio::time::sleep(Duration::from_millis(300)).await;
            continue;
        }
        let client = match NatsClient::connect(&state.nats_url).await {
            Ok(client) => client,
            Err(err) => {
                warn!("nats core combiner connect failed, reconnecting: {}", err);
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        let mut subscriber = match client.subscribe_core(&source, None).await {
            Ok(subscriber) => subscriber,
            Err(err) => {
                warn!("nats core combiner subscribe failed, reconnecting: {}", err);
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        info!("nats core combiner subscribed to {}", source);
        state.mark_core_ready(ready_mask);
        while matches!(state.runtime_mode(), RuntimeMode::Active) {
            match tokio::time::timeout(Duration::from_millis(500), subscriber.next()).await {
                Ok(Some(message)) => {
                    if let Err(err) =
                        process_core_output_message(&state, &client, &source, &target, message)
                            .await
                    {
                        warn!("nats core combiner processing failed: {}", err);
                    }
                }
                Ok(None) => break,
                Err(_) => {}
            }
        }
    }
}

async fn process_core_output_message(
    state: &AppState,
    client: &NatsClient,
    source: &str,
    target: &str,
    message: async_nats::Message,
) -> Result<()> {
    let body = message.payload.to_vec();
    let body_json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let route = route_from_output_source(combiner_config(&state.config)?, source);
    publish_to_target(state, client, target, body).await?;
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
    Ok(())
}

async fn publish_to_target(
    state: &AppState,
    client: &NatsClient,
    target: &str,
    body: Vec<u8>,
) -> Result<()> {
    if state.config.mode.is_core() {
        client.publish_core(target, body).await
    } else {
        client.publish(target, body).await
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
    if state.config.mode.is_core() {
        return Ok(());
    }
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
    let green_task = if state.config.mode.is_core() {
        tokio::spawn(run_core_combine_loop(
            green,
            output.clone(),
            state.clone(),
            CORE_READY_GREEN_OUTPUT,
        ))
    } else {
        tokio::spawn(run_combine_loop(green, output.clone(), state.clone()))
    };
    let blue_task = if state.config.mode.is_core() {
        tokio::spawn(run_core_combine_loop(
            blue,
            output,
            state,
            CORE_READY_BLUE_OUTPUT,
        ))
    } else {
        tokio::spawn(run_combine_loop(blue, output, state))
    };
    green_task.await??;
    blue_task.await??;
    Ok(())
}
