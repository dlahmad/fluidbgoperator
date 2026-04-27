use anyhow::{Result, bail};
use axum::{
    Router,
    routing::{get, post},
};
use fluidbg_plugin_sdk::{PluginInceptorRuntime, PluginRole};
use tracing::info;

mod amqp;
mod assignments;
mod combiner;
mod config;
mod filtering;
mod input;
mod lifecycle;
mod manager;
mod writer;

use combiner::run_combiner;
use config::{AppState, has_role, load_config};
use input::run_input_pipeline;
use lifecycle::{
    cleanup_handler, drain_handler, drain_status_handler, health, prepare_handler,
    traffic_shift_handler,
};
use writer::write_handler;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    if std::env::var("FLUIDBG_PLUGIN_MANAGER").as_deref() == Ok("true") {
        return run_manager().await;
    }

    let config = load_config()?;
    let runtime = PluginInceptorRuntime::from_env();
    let roles = runtime.roles().to_vec();
    if roles.is_empty() {
        bail!("no active roles configured via FLUIDBG_ACTIVE_ROLES");
    }

    let amqp_url = config
        .amqp_url
        .clone()
        .unwrap_or_else(|| "amqp://fluidbg:fluidbg@rabbitmq.fluidbg-system:5672/%2f".to_string());
    let state = AppState::new(config.clone(), roles.clone(), amqp_url.clone(), runtime);

    info!("rabbitmq plugin starting with roles {:?}", roles);

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
        bail!("unsupported RabbitMQ role set");
    };

    let (_, worker_result) = tokio::join!(server, worker);
    worker_result??;
    Ok(())
}

async fn run_manager() -> Result<()> {
    let state = manager::manager_state_from_env()?;
    let app = Router::new()
        .route("/health", get(manager::health))
        .route("/manager/prepare", post(manager::prepare_handler))
        .route("/manager/cleanup", post(manager::cleanup_handler))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("0.0.0.0:9090").await?;
    axum::serve(listener, app).await?;
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
    fn green_route_notifies_but_does_not_register_operator_case() {
        let route = TrafficRoute::Green;
        assert_eq!(route.as_str(), "green");
        assert!(!route.should_register_case());
    }

    #[test]
    fn combiner_route_comes_from_source_queue() {
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
        assert_eq!(
            route_from_output_source(&combiner, "results-other"),
            TrafficRoute::Unknown
        );
    }
}
