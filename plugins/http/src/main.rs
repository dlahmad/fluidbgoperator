use std::sync::{
    Arc,
    atomic::{AtomicU8, AtomicUsize},
};

use anyhow::Result;
use fluidbg_plugin_sdk::{PluginInceptorRuntime, traffic_percent_from_env};
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

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config = load_config()?;
    let runtime = PluginInceptorRuntime::from_env();
    let port = config.listen_port();

    info!(
        "http plugin starting on port {}: roles={:?}, mode={}, real={:?}, target={:?}, envVar={:?}, writeEnvVar={:?}",
        port,
        runtime.roles(),
        runtime.mode(),
        config.real_endpoint,
        config.target_url,
        config.env_var_name,
        config.write_env_var
    );

    let state = AppState {
        config,
        runtime,
        mode: Arc::new(AtomicU8::new(RuntimeMode::Idle as u8)),
        active_requests: Arc::new(AtomicUsize::new(0)),
        traffic_percent: Arc::new(AtomicUsize::new(traffic_percent_from_env() as usize)),
    };

    let app = axum::Router::new()
        .route("/health", axum::routing::get(health))
        .route("/prepare", axum::routing::post(prepare_handler))
        .route("/activate", axum::routing::post(activate_handler))
        .route("/drain", axum::routing::post(drain_handler))
        .route("/cleanup", axum::routing::post(cleanup_handler))
        .route("/drain-status", axum::routing::get(drain_status))
        .route("/traffic", axum::routing::post(traffic_shift_handler))
        .route("/write", axum::routing::post(write_handler))
        .fallback(proxy_handler)
        .with_state(state);

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("http plugin listening on {}", addr);
    axum::serve(listener, app).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue};
    use fluidbg_plugin_sdk::{FilterCondition, NotificationFilter, TestIdSelector, TrafficRoute};

    use crate::config::{Config, resolve_runtime_endpoint_with};
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
            proxy_port: None,
            port: None,
            real_endpoint: Some("http://upstream".to_string()),
            target_url: None,
            green_endpoint: None,
            blue_endpoint: None,
            env_var_name: None,
            write_env_var: None,
            ingress_port: None,
            egress_port: None,
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
                    payload: None,
                },
            ],
            ingress: None,
            egress: None,
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
            proxy_port: None,
            port: None,
            real_endpoint: Some("http://blue".to_string()),
            target_url: None,
            green_endpoint: None,
            blue_endpoint: None,
            env_var_name: None,
            write_env_var: None,
            ingress_port: None,
            egress_port: None,
            test_id: None,
            r#match: Vec::new(),
            filters: Vec::new(),
            ingress: None,
            egress: None,
        };

        assert_eq!(config.write_target(), Some("http://blue".to_string()));
    }

    #[test]
    fn splitter_route_selects_specific_target() {
        let config = Config {
            proxy_port: None,
            port: None,
            real_endpoint: Some("http://fallback".to_string()),
            target_url: None,
            green_endpoint: Some("http://green".to_string()),
            blue_endpoint: Some("http://blue".to_string()),
            env_var_name: None,
            write_env_var: None,
            ingress_port: None,
            egress_port: None,
            test_id: None,
            r#match: Vec::new(),
            filters: Vec::new(),
            ingress: None,
            egress: None,
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
