use std::time::Duration;

use anyhow::Result;
use fluidbg_plugin_sdk::PluginRole;
use serde_json::Value;
use tracing::{info, warn};

use crate::config::{AppState, RuntimeMode, combiner_config, has_role, observer_config, required};
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
            RuntimeMode::Idle | RuntimeMode::Draining => {
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
