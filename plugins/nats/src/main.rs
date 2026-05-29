use anyhow::{Result, bail};
use axum::{
    Router,
    routing::{get, post},
};
use fluidbg_plugin_sdk::{
    ControlPlaneServerTls, PluginInceptorRuntime, QueueWorkerRole, queue_worker_role,
    serve_control_plane,
};
use tracing::info;

mod assignments;
mod combiner;
mod config;
mod filtering;
mod input;
mod lifecycle;
mod manager;
mod nats;
mod writer;

use combiner::run_combiner;
use config::{AppState, load_config, nats_url_from_env};
use input::run_input_pipeline;
use lifecycle::{
    activate_handler, cleanup_handler, drain_handler, drain_status_handler, health,
    prepare_handler, traffic_shift_handler,
};
use writer::write_handler;

const DEFAULT_LOG_FILTER: &str = "warn,fluidbg_nats=info";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(DEFAULT_LOG_FILTER)),
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
    let nats_url = nats_url_from_env()?;
    let state = AppState::new(config, roles.clone(), runtime, nats_url);
    info!("nats plugin starting with roles {:?}", roles);

    let app = Router::new()
        .route("/health", get(health))
        .route("/prepare", post(prepare_handler))
        .route("/activate", post(activate_handler))
        .route("/drain", post(drain_handler))
        .route("/drain-status", get(drain_status_handler))
        .route("/traffic", post(traffic_shift_handler))
        .route("/cleanup", post(cleanup_handler))
        .route("/write", post(write_handler))
        .with_state(state.clone());

    let server = tokio::spawn(async move {
        if let Err(err) =
            serve_control_plane(app, "0.0.0.0:9090", ControlPlaneServerTls::from_env()).await
        {
            tracing::error!("server error: {}", err);
        }
    });

    let worker = match queue_worker_role(&roles).map_err(anyhow::Error::msg)? {
        Some(QueueWorkerRole::Combiner) => tokio::spawn(async move { run_combiner(state).await }),
        Some(QueueWorkerRole::Input) => {
            tokio::spawn(async move { run_input_pipeline(state).await })
        }
        None => tokio::spawn(async { Ok::<(), anyhow::Error>(()) }),
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
        .route("/manager/sync", post(manager::sync_handler))
        .with_state(state);
    serve_control_plane(app, "0.0.0.0:9090", ControlPlaneServerTls::from_env()).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use fluidbg_plugin_sdk::{PluginRole, QueueWorkerRole, queue_worker_role};

    #[test]
    fn observer_and_writer_are_additive_to_movement_roles() {
        assert_eq!(
            queue_worker_role(&[PluginRole::Splitter, PluginRole::Observer]).unwrap(),
            Some(QueueWorkerRole::Input)
        );
        assert_eq!(
            queue_worker_role(&[
                PluginRole::Combiner,
                PluginRole::Observer,
                PluginRole::Writer
            ])
            .unwrap(),
            Some(QueueWorkerRole::Combiner)
        );
    }

    #[test]
    fn conflicting_movement_roles_are_rejected() {
        assert!(queue_worker_role(&[PluginRole::Splitter, PluginRole::Combiner]).is_err());
    }
}
