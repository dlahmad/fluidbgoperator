use std::time::Duration;

use anyhow::Result;
use fluidbg_plugin_sdk::{PluginRole, TrafficRoute};
use futures_lite::StreamExt;
use lapin::BasicProperties;
use lapin::options::{BasicAckOptions, BasicConsumeOptions};
use lapin::types::FieldTable;
use serde_json::Value;
use tracing::{info, warn};

use crate::amqp::{connect_with_retry, declare_queue, publish_confirmed};
use crate::config::{AppState, RuntimeMode, combiner_config, has_role, observer_config, required};
use crate::filtering::{
    extract_test_id, matches_filter, notify_observer, route_from_output_source,
};

async fn run_combine_loop_once(
    source_queue: String,
    result_queue: String,
    state: AppState,
) -> Result<()> {
    let conn = connect_with_retry(&state.amqp_url).await?;
    let consume_channel = conn.create_channel().await?;
    let publish_channel = conn.create_channel().await?;
    declare_queue(&consume_channel, &source_queue).await?;
    declare_queue(&publish_channel, &result_queue).await?;
    let combiner = combiner_config(&state.config)?;

    let mut consumer = consume_channel
        .basic_consume(
            source_queue.as_str().into(),
            "fluidbg-rabbitmq-combiner".into(),
            BasicConsumeOptions::default(),
            FieldTable::default(),
        )
        .await?;

    while let Some(delivery) = consumer.next().await {
        match state.runtime_mode() {
            RuntimeMode::Idle => {
                tokio::time::sleep(Duration::from_millis(300)).await;
                continue;
            }
            RuntimeMode::Draining => {
                tokio::time::sleep(Duration::from_millis(300)).await;
                continue;
            }
            RuntimeMode::Active => {}
        }
        let delivery = delivery?;
        let headers = delivery.properties.headers().clone().unwrap_or_default();
        let body_json: Value = serde_json::from_slice(&delivery.data).unwrap_or(Value::Null);
        let route = route_from_output_source(combiner, &source_queue);
        publish_confirmed(
            &publish_channel,
            &result_queue,
            &delivery.data,
            BasicProperties::default().with_headers(headers.clone()),
        )
        .await?;

        if has_role(&state.roles, PluginRole::Observer)
            && let Some(observer) = observer_config(&state.config)
            && matches_filter(&observer.r#match, &body_json, &headers)
            && let Some(selector) = &observer.test_id
            && let Some(test_id) = extract_test_id(selector, &body_json, &headers)
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
        delivery.ack(BasicAckOptions::default()).await?;
    }

    Ok(())
}

async fn run_combine_loop(
    source_queue: String,
    result_queue: String,
    state: AppState,
) -> Result<()> {
    loop {
        if matches!(state.runtime_mode(), RuntimeMode::Idle) {
            tokio::time::sleep(Duration::from_millis(300)).await;
            continue;
        }
        match run_combine_loop_once(source_queue.clone(), result_queue.clone(), state.clone()).await
        {
            Ok(()) => {
                warn!(
                    "rabbitmq combine loop for '{}' ended, reconnecting",
                    source_queue
                );
            }
            Err(err) => {
                warn!(
                    "rabbitmq combine loop for '{}' failed, reconnecting: {}",
                    source_queue, err
                );
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
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

#[allow(dead_code)]
fn _route_type_marker(route: TrafficRoute) -> TrafficRoute {
    route
}
