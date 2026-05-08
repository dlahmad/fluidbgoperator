use std::sync::{
    Arc,
    atomic::{AtomicU8, AtomicUsize},
};

use anyhow::{Context, Result};
use fluidbg_plugin_sdk::{
    ControlPlaneServerTls, PluginInceptorRuntime, env_port, serve_control_plane,
    traffic_percent_from_env,
};
use tracing::info;

mod config;
mod filters;
mod handlers;
mod state;

use config::load_config;
use handlers::{
    activate_handler, cleanup_handler, drain_handler, drain_status, health, prepare_handler,
    proxy_handler, traffic_shift_handler, write_handler,
};
use state::{AppState, RuntimeMode};

const DEFAULT_LOG_FILTER: &str = "warn,fluidbg_http=info";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(DEFAULT_LOG_FILTER)),
        )
        .init();

    let config = load_config()?;
    config.validate()?;
    let client = config.http_client()?;
    let runtime = PluginInceptorRuntime::from_env();
    let port = config.listen_port();

    info!(
        "http plugin starting on port {}: roles={:?}, real={:?}, target={:?}, envVar={:?}, writeEnvVar={:?}",
        port,
        runtime.roles(),
        config.real_endpoint,
        config.target_url,
        config.env_var_name,
        config.write_env_var
    );

    let state = AppState {
        config,
        runtime,
        client,
        mode: Arc::new(AtomicU8::new(RuntimeMode::Idle as u8)),
        active_requests: Arc::new(AtomicUsize::new(0)),
        traffic_percent: Arc::new(AtomicUsize::new(traffic_percent_from_env() as usize)),
    };

    let app = router(state.clone());
    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    let control_plane_tls = ControlPlaneServerTls::from_env();
    let control_plane_tls_port = env_port("FLUIDBG_CONTROL_PLANE_TLS_PORT", 9443);

    if state.config.tls.inbound.enabled {
        let https_port = state.config.inbound_https_port();
        if !(control_plane_tls.enabled && https_port == control_plane_tls_port) {
            let https_addr = std::net::SocketAddr::from(([0, 0, 0, 0], https_port));
            let cert = state
                .config
                .tls
                .inbound
                .cert_path
                .as_deref()
                .context("tls.inbound.certPath missing")?;
            let key = state
                .config
                .tls
                .inbound
                .key_path
                .as_deref()
                .context("tls.inbound.keyPath missing")?;
            let tls_config = axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key)
                .await
                .with_context(|| {
                    format!("failed to load inbound TLS certificate {cert} and key {key}")
                })?;
            let https_app = router(state.clone());
            tokio::spawn(async move {
                info!("http plugin HTTPS traffic listener on {}", https_addr);
                if let Err(err) = axum_server::bind_rustls(https_addr, tls_config)
                    .serve(https_app.into_make_service())
                    .await
                {
                    tracing::error!("http plugin HTTPS listener failed: {}", err);
                }
            });
        }
    }

    if control_plane_tls.enabled {
        let control_addr = std::net::SocketAddr::from(([0, 0, 0, 0], control_plane_tls_port));
        let control_app = router(state.clone());
        tokio::spawn(async move {
            info!(
                "http plugin HTTPS control-plane listener on {}",
                control_addr
            );
            if let Err(err) =
                serve_control_plane(control_app, &control_addr.to_string(), control_plane_tls).await
            {
                tracing::error!("http plugin HTTPS control-plane listener failed: {}", err);
            }
        });
    }

    info!("http plugin HTTP control/traffic listener on {}", addr);
    serve_control_plane(
        app,
        &addr.to_string(),
        ControlPlaneServerTls {
            enabled: false,
            cert_path: None,
            key_path: None,
        },
    )
    .await?;

    Ok(())
}

fn router(state: AppState) -> axum::Router {
    axum::Router::new()
        .route("/health", axum::routing::get(health))
        .route("/prepare", axum::routing::post(prepare_handler))
        .route("/activate", axum::routing::post(activate_handler))
        .route("/drain", axum::routing::post(drain_handler))
        .route("/cleanup", axum::routing::post(cleanup_handler))
        .route("/drain-status", axum::routing::get(drain_status))
        .route("/traffic", axum::routing::post(traffic_shift_handler))
        .route("/write", axum::routing::post(write_handler))
        .fallback(proxy_handler)
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue};
    use fluidbg_plugin_sdk::{FilterCondition, NotificationFilter, TestIdSelector, TrafficRoute};

    use crate::config::{ClientTlsConfig, Config, TlsConfig, resolve_runtime_endpoint_with};
    use crate::filters::{extract_test_id, matching_filter};

    #[test]
    fn extracts_test_id_from_json_body_without_route_content() {
        let selector = TestIdSelector {
            field: Some("http.body".to_string()),
            json_path: Some("$.orderId".to_string()),
            path_segment: None,
            value: None,
        };
        let body = serde_json::json!({
            "orderId": "order-17",
            "status": "created"
        });
        let headers = HeaderMap::new();

        assert_eq!(
            extract_test_id(&selector, &body, "/orders", &headers),
            Some("order-17".to_string())
        );
    }

    #[test]
    fn matching_filter_uses_filter_specific_conditions() {
        let config = Config {
            port: None,
            proxy_protocol: None,
            write_protocol: None,
            real_endpoint: Some("http://upstream".to_string()),
            target_url: None,
            green_endpoint: None,
            blue_endpoint: None,
            env_var_name: None,
            write_env_var: None,
            verifier_endpoint: None,
            mock_path: None,
            test_id: None,
            r#match: Vec::new(),
            filters: vec![
                NotificationFilter {
                    r#match: vec![FilterCondition {
                        field: "http.path".to_string(),
                        equals: Some("/ignored".to_string()),
                        matches: None,
                        json_path: None,
                    }],
                    notify_path: Some("/ignored/{testId}".to_string()),
                    mock_path: None,
                    payload: None,
                },
                NotificationFilter {
                    r#match: vec![FilterCondition {
                        field: "http.header.X-Event".to_string(),
                        equals: Some("order-created".to_string()),
                        matches: None,
                        json_path: None,
                    }],
                    notify_path: Some("/observe/{testId}/orders".to_string()),
                    mock_path: None,
                    payload: None,
                },
            ],
            ingress: None,
            egress: None,
            tls: TlsConfig::default(),
            client_tls: ClientTlsConfig::default(),
        };
        let mut headers = HeaderMap::new();
        headers.insert("X-Event", HeaderValue::from_static("order-created"));

        let filter = matching_filter(
            &config,
            "POST",
            "/orders",
            &headers,
            &serde_json::json!({"orderId": "order-17"}),
        )
        .expect("expected second filter to match");

        assert_eq!(
            filter.notify_path.as_deref(),
            Some("/observe/{testId}/orders")
        );
    }

    #[test]
    fn route_metadata_is_plugin_supplied_not_payload_supplied() {
        let notification = fluidbg_plugin_sdk::ObservationNotification {
            test_id: "order-17",
            inception_point: "incoming-orders",
            route: TrafficRoute::Blue.as_str(),
            payload: &serde_json::json!({"orderId": "order-17"}),
        };

        let encoded = serde_json::to_value(notification).unwrap();

        assert_eq!(encoded["route"], "blue");
        assert!(encoded["payload"].get("route").is_none());
    }

    #[test]
    fn write_target_defaults_to_proxy_target() {
        let config = Config {
            port: None,
            proxy_protocol: None,
            write_protocol: None,
            real_endpoint: Some("http://blue".to_string()),
            target_url: None,
            green_endpoint: None,
            blue_endpoint: None,
            env_var_name: None,
            write_env_var: None,
            verifier_endpoint: None,
            mock_path: None,
            test_id: None,
            r#match: Vec::new(),
            filters: Vec::new(),
            ingress: None,
            egress: None,
            tls: TlsConfig::default(),
            client_tls: ClientTlsConfig::default(),
        };

        assert_eq!(config.write_target(), Some("http://blue".to_string()));
    }

    #[test]
    fn splitter_route_selects_specific_target() {
        let config = Config {
            port: None,
            proxy_protocol: None,
            write_protocol: None,
            real_endpoint: Some("http://fallback".to_string()),
            target_url: None,
            green_endpoint: Some("http://green".to_string()),
            blue_endpoint: Some("http://blue".to_string()),
            env_var_name: None,
            write_env_var: None,
            verifier_endpoint: None,
            mock_path: None,
            test_id: None,
            r#match: Vec::new(),
            filters: Vec::new(),
            ingress: None,
            egress: None,
            tls: TlsConfig::default(),
            client_tls: ClientTlsConfig::default(),
        };

        assert_eq!(
            config.routed_proxy_target(TrafficRoute::Green),
            Some("http://green".to_string())
        );
        assert_eq!(
            config.routed_proxy_target(TrafficRoute::Blue),
            Some("http://blue".to_string())
        );
    }

    #[test]
    fn proxy_target_can_reference_operator_test_container_url() {
        assert_eq!(
            resolve_runtime_endpoint_with(
                "{{testContainerUrl}}/audit",
                "http://fluidbg-test-verifier:8080"
            ),
            "http://fluidbg-test-verifier:8080/audit"
        );
    }
}
