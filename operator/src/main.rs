use std::sync::Arc;
use std::time::Duration;

use tracing::info;

use fluidbg_operator::controller::AuthConfig;
use fluidbg_operator::http_api;
use fluidbg_operator::inception::InceptionTracker;
use fluidbg_operator::state_store::{StateStore, memory::MemoryStore, postgres::PostgresStore};
use fluidbg_operator::state_store::{azure_identity, cosmos::CosmosStore};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "builtin-plugins") {
        run_builtin_plugins_hook(&args).await;
        return;
    }

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
    let orphan_store = store.clone();
    let orphan_client = client.clone();
    let orphan_ns = namespace.clone();
    let orphan_interval = Duration::from_secs(
        std::env::var("FLUIDBG_ORPHAN_CLEANUP_INTERVAL_SECONDS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(60),
    );
    tokio::spawn(async move {
        fluidbg_operator::controller::run_controller(ctrl_client, ctrl_ns, auth, ctrl_store).await;
    });

    tokio::spawn(async move {
        fluidbg_operator::controller::run_orphan_cleanup(
            orphan_client,
            orphan_ns,
            orphan_store,
            orphan_interval,
        )
        .await;
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

async fn run_builtin_plugins_hook(args: &[String]) {
    let mode = args
        .get(1)
        .unwrap_or_else(|| panic!("builtin-plugins requires mode: apply or delete"));
    let manifest_path = args
        .get(2)
        .unwrap_or_else(|| panic!("builtin-plugins requires manifest path"));
    fluidbg_operator::builtin_plugins::run(mode, manifest_path)
        .await
        .expect("builtin plugin hook failed");
}

async fn build_state_store() -> Arc<dyn StateStore> {
    match std::env::var("FLUIDBG_STATE_STORE_TYPE")
        .unwrap_or_else(|_| "memory".to_string())
        .as_str()
    {
        "memory" => Arc::new(MemoryStore::new()),
        "postgres" => {
            let table = std::env::var("FLUIDBG_POSTGRES_TABLE")
                .unwrap_or_else(|_| "fluidbg_cases".to_string());
            let auth_mode = std::env::var("FLUIDBG_POSTGRES_AUTH_MODE")
                .unwrap_or_else(|_| "password".to_string());
            let store = if auth_mode == "workloadIdentity" {
                PostgresStore::new_workload_identity(
                    &required_env("FLUIDBG_POSTGRES_HOST"),
                    &required_env("FLUIDBG_POSTGRES_DATABASE"),
                    &required_env("FLUIDBG_POSTGRES_USER"),
                    &table,
                    azure_identity::workload_identity_from_env("FLUIDBG_POSTGRES"),
                )
                .await
            } else {
                let url = required_env("FLUIDBG_POSTGRES_URL");
                PostgresStore::new(&url, &table).await
            }
            .expect("failed to connect to postgres state store");
            store
                .migrate()
                .await
                .expect("failed to migrate postgres state store");
            Arc::new(store)
        }
        "cosmosdb" => {
            let database = required_env("FLUIDBG_COSMOS_DATABASE");
            let container = required_env("FLUIDBG_COSMOS_CONTAINER");
            let auth_mode = std::env::var("FLUIDBG_COSMOS_AUTH_MODE")
                .unwrap_or_else(|_| "connectionString".to_string());
            let store = if auth_mode == "workloadIdentity" {
                CosmosStore::new_workload_identity(
                    &required_env("FLUIDBG_COSMOS_ENDPOINT"),
                    &database,
                    &container,
                    azure_identity::workload_identity_from_env("FLUIDBG_COSMOS"),
                )
            } else if let Ok(connection_string) = std::env::var("FLUIDBG_COSMOS_CONNECTION_STRING")
            {
                CosmosStore::from_connection_string(&connection_string, &database, &container)
                    .expect("failed to parse cosmos connection string")
            } else {
                CosmosStore::new_master_key(
                    &required_env("FLUIDBG_COSMOS_ENDPOINT"),
                    &database,
                    &container,
                    &required_env("FLUIDBG_COSMOS_ACCOUNT_KEY"),
                )
            };
            store
                .validate_container()
                .await
                .expect("failed to validate cosmos state store container");
            Arc::new(store)
        }
        other => panic!("unsupported FLUIDBG_STATE_STORE_TYPE: {other}"),
    }
}

fn required_env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is required"))
}
