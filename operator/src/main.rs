use std::sync::Arc;

use tracing::info;

use fluidbg_operator::controller::AuthConfig;
use fluidbg_operator::http_api;
use fluidbg_operator::inception::InceptionTracker;
use fluidbg_operator::state_store::{StateStore, memory::MemoryStore, postgres::PostgresStore};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    info!("fluidbg operator v0.1 starting");

    let store = build_state_store().await;

    let namespace =
        std::env::var("FLUIDBG_NAMESPACE").unwrap_or_else(|_| "fluidbg-system".to_string());
    let signing_secret_namespace = std::env::var("FLUIDBG_AUTH_SIGNING_SECRET_NAMESPACE")
        .unwrap_or_else(|_| "fluidbg-system".to_string());
    let auth = AuthConfig {
        signing_secret_namespace,
        signing_secret_name: std::env::var("FLUIDBG_AUTH_SIGNING_SECRET_NAME")
            .unwrap_or_else(|_| "fluidbg-operator-auth".to_string()),
        signing_secret_key: std::env::var("FLUIDBG_AUTH_SIGNING_SECRET_KEY")
            .unwrap_or_else(|_| "signing-key".to_string()),
    };

    let client = kube::Client::try_default()
        .await
        .expect("failed to create kubernetes client");

    let tracker = InceptionTracker::new(
        store.clone(),
        tokio::time::Duration::from_secs(5),
        tokio::time::Duration::from_secs(10),
    );

    let app = http_api::router(
        store.clone(),
        client.clone(),
        namespace.clone(),
        auth.clone(),
    );

    let ctrl_store = store.clone();
    let ctrl_client = client.clone();
    let ctrl_ns = namespace.clone();
    tokio::spawn(async move {
        fluidbg_operator::controller::run_controller(ctrl_client, ctrl_ns, auth, ctrl_store).await;
    });

    tokio::spawn(async move {
        tracker.run().await;
    });

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8090")
        .await
        .expect("failed to bind to port 8090");
    info!("operator API listening on 0.0.0.0:8090");
    axum::serve(listener, app)
        .await
        .expect("operator API server error");
}

async fn build_state_store() -> Arc<dyn StateStore> {
    match std::env::var("FLUIDBG_STATE_STORE_TYPE")
        .unwrap_or_else(|_| "memory".to_string())
        .as_str()
    {
        "memory" => Arc::new(MemoryStore::new()),
        "postgres" => {
            let url = std::env::var("FLUIDBG_POSTGRES_URL")
                .expect("FLUIDBG_POSTGRES_URL is required when FLUIDBG_STATE_STORE_TYPE=postgres");
            let table = std::env::var("FLUIDBG_POSTGRES_TABLE")
                .unwrap_or_else(|_| "fluidbg_cases".to_string());
            let store = PostgresStore::new(&url, &table)
                .await
                .expect("failed to connect to postgres state store");
            store
                .migrate()
                .await
                .expect("failed to migrate postgres state store");
            Arc::new(store)
        }
        other => panic!("unsupported FLUIDBG_STATE_STORE_TYPE: {other}"),
    }
}
