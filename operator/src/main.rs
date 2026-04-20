use std::sync::Arc;

use tracing::info;

use fluidbg_operator::http_api;
use fluidbg_operator::inception::InceptionTracker;
use fluidbg_operator::state_store::memory::MemoryStore;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    info!("fluidbg operator v0.1 starting");

    let store: Arc<dyn fluidbg_operator::state_store::StateStore> = Arc::new(MemoryStore::new());

    let tracker = InceptionTracker::new(
        store.clone(),
        tokio::time::Duration::from_secs(5),
        tokio::time::Duration::from_secs(10),
        "http://localhost:8080".to_string(),
    );

    let app = http_api::router(store);

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
