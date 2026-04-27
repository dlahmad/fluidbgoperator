use anyhow::{Result, bail};
use axum::{
    Router,
    routing::{get, post},
};
use fluidbg_plugin_sdk::{PluginRole, PluginRuntime};
use tracing::info;

mod assignments;
mod combiner;
mod config;
mod filtering;
mod input;
mod lifecycle;
mod servicebus;
mod writer;

use combiner::run_combiner;
use config::{AppState, has_role, load_config};
use input::run_input_pipeline;
use lifecycle::{
    cleanup_handler, drain_handler, drain_status_handler, health, prepare_handler,
    traffic_shift_handler,
};
use servicebus::ServiceBusClient;
use writer::write_handler;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config = load_config()?;
    let runtime = PluginRuntime::from_env();
    let roles = runtime.roles().to_vec();
    if roles.is_empty() {
        bail!("no active roles configured via FLUIDBG_ACTIVE_ROLES");
    }

    let service_bus = ServiceBusClient::from_config(&config)?;
    let state = AppState::new(config, roles.clone(), runtime, service_bus);

    info!("azure service bus plugin starting with roles {:?}", roles);

    let app = Router::new()
        .route("/health", get(health))
        .route("/prepare", post(prepare_handler))
        .route("/drain", post(drain_handler))
        .route("/drain-status", get(drain_status_handler))
        .route("/traffic", post(traffic_shift_handler))
        .route("/cleanup", post(cleanup_handler))
        .route("/write", post(write_handler))
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind("0.0.0.0:9090").await?;
    let server = tokio::spawn(async move {
        if let Err(err) = axum::serve(listener, app).await {
            tracing::error!("server error: {}", err);
        }
    });

    let worker = if has_role(&roles, PluginRole::Writer) {
        tokio::spawn(async { Ok::<(), anyhow::Error>(()) })
    } else if has_role(&roles, PluginRole::Combiner) {
        tokio::spawn(async move { run_combiner(state).await })
    } else if has_role(&roles, PluginRole::Duplicator)
        || has_role(&roles, PluginRole::Splitter)
        || has_role(&roles, PluginRole::Observer)
        || has_role(&roles, PluginRole::Consumer)
    {
        tokio::spawn(async move { run_input_pipeline(state).await })
    } else {
        bail!("unsupported Azure Service Bus role set");
    };

    let (_, worker_result) = tokio::join!(server, worker);
    worker_result??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use fluidbg_plugin_sdk::TrafficRoute;

    use crate::config::CombinerConfig;
    use crate::filtering::route_from_output_source;

    #[test]
    fn duplicator_route_is_reported_as_both_and_registered() {
        let route = TrafficRoute::Both;
        assert_eq!(route.as_str(), "both");
        assert!(route.should_register_case());
    }

    #[test]
    fn combiner_route_comes_from_service_bus_source_queue() {
        let combiner = CombinerConfig {
            output_queue: Some("results".to_string()),
            green_output_queue: Some("results-green".to_string()),
            blue_output_queue: Some("results-blue".to_string()),
            green_output_queue_env_var: None,
            blue_output_queue_env_var: None,
        };

        assert_eq!(
            route_from_output_source(&combiner, "results-green"),
            TrafficRoute::Green
        );
        assert_eq!(
            route_from_output_source(&combiner, "results-blue"),
            TrafficRoute::Blue
        );
    }
}
