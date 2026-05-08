use anyhow::{Context, Result, bail};
use fluidbg_plugin_sdk::{FilterCondition, NotificationFilter, TestIdSelector, TrafficRoute};
use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Config {
    #[serde(default)]
    pub(crate) port: Option<u16>,
    #[serde(default)]
    pub(crate) proxy_protocol: Option<ProxyProtocol>,
    #[serde(default)]
    pub(crate) write_protocol: Option<ProxyProtocol>,
    #[serde(default)]
    pub(crate) real_endpoint: Option<String>,
    #[serde(default)]
    pub(crate) target_url: Option<String>,
    #[serde(default)]
    pub(crate) green_endpoint: Option<String>,
    #[serde(default)]
    pub(crate) blue_endpoint: Option<String>,
    #[serde(default)]
    pub(crate) env_var_name: Option<String>,
    #[serde(default)]
    pub(crate) write_env_var: Option<String>,
    #[serde(default)]
    pub(crate) verifier_endpoint: Option<String>,
    #[serde(default)]
    pub(crate) mock_path: Option<String>,
    #[serde(default)]
    pub(crate) test_id: Option<TestIdSelector>,
    #[serde(default)]
    pub(crate) r#match: Vec<FilterCondition>,
    #[serde(default)]
    pub(crate) filters: Vec<NotificationFilter>,
    #[serde(default)]
    pub(crate) ingress: Option<DirectionConfig>,
    #[serde(default)]
    pub(crate) egress: Option<DirectionConfig>,
    #[serde(default)]
    pub(crate) tls: TlsConfig,
    #[serde(default)]
    pub(crate) client_tls: ClientTlsConfig,
}

impl Config {
    pub(crate) fn listen_port(&self) -> u16 {
        self.port.unwrap_or(9090)
    }

    pub(crate) fn inbound_https_port(&self) -> u16 {
        self.tls.inbound.port.unwrap_or(9443)
    }

    pub(crate) fn routed_proxy_target(&self, route: TrafficRoute) -> Option<String> {
        match route {
            TrafficRoute::Blue => self
                .blue_endpoint
                .as_deref()
                .or(self.real_endpoint.as_deref()),
            TrafficRoute::Green => self
                .green_endpoint
                .as_deref()
                .or(self.real_endpoint.as_deref()),
            _ => self.real_endpoint.as_deref(),
        }
        .map(resolve_runtime_endpoint)
    }

    pub(crate) fn write_target(&self) -> Option<String> {
        self.target_url
            .as_deref()
            .or(self.real_endpoint.as_deref())
            .map(resolve_runtime_endpoint)
    }

    pub(crate) fn verifier_base_url(&self, runtime_test_container_url: &str) -> String {
        self.verifier_endpoint
            .as_deref()
            .map(resolve_runtime_endpoint)
            .unwrap_or_else(|| runtime_test_container_url.to_string())
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if matches!(self.proxy_protocol, Some(ProxyProtocol::Https))
            || matches!(self.write_protocol, Some(ProxyProtocol::Https))
        {
            self.tls.inbound.validate()?;
        }
        if self.tls.inbound.enabled {
            self.tls.inbound.validate()?;
        }
        Ok(())
    }

    pub(crate) fn http_client(&self) -> Result<reqwest::Client> {
        let mut builder = reqwest::Client::builder();
        if self.client_tls.insecure_skip_verify {
            builder = builder.danger_accept_invalid_certs(true);
        }
        if let Some(path) = &self.client_tls.ca_cert_path {
            let pem = std::fs::read(path)
                .with_context(|| format!("failed to read client CA certificate {path}"))?;
            let cert = reqwest::Certificate::from_pem(&pem)
                .with_context(|| format!("failed to parse client CA certificate {path}"))?;
            builder = builder.add_root_certificate(cert);
        }
        builder.build().context("failed to build HTTP client")
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ProxyProtocol {
    Http,
    Https,
}

#[derive(Debug, Default, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TlsConfig {
    #[serde(default)]
    pub(crate) inbound: InboundTlsConfig,
}

#[derive(Debug, Default, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InboundTlsConfig {
    #[serde(default)]
    pub(crate) enabled: bool,
    #[serde(default)]
    pub(crate) port: Option<u16>,
    #[serde(default)]
    pub(crate) cert_path: Option<String>,
    #[serde(default)]
    pub(crate) key_path: Option<String>,
}

impl InboundTlsConfig {
    fn validate(&self) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        if self.port.unwrap_or(9443) == 0 {
            bail!("tls.inbound.port must be greater than zero");
        }
        if self.cert_path.as_deref().unwrap_or_default().is_empty() {
            bail!("tls.inbound.certPath is required when inbound TLS is enabled");
        }
        if self.key_path.as_deref().unwrap_or_default().is_empty() {
            bail!("tls.inbound.keyPath is required when inbound TLS is enabled");
        }
        Ok(())
    }
}

#[derive(Debug, Default, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClientTlsConfig {
    #[serde(default)]
    pub(crate) ca_cert_path: Option<String>,
    #[serde(default)]
    pub(crate) insecure_skip_verify: bool,
}

fn resolve_runtime_endpoint(endpoint: &str) -> String {
    let test_container_url = std::env::var("FLUIDBG_TEST_CONTAINER_URL").unwrap_or_default();
    resolve_runtime_endpoint_with(endpoint, &test_container_url)
}

pub(crate) fn resolve_runtime_endpoint_with(endpoint: &str, test_container_url: &str) -> String {
    endpoint
        .replace("{{testContainerUrl}}", test_container_url)
        .replace("{testContainerUrl}", test_container_url)
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DirectionConfig {
    #[serde(default)]
    pub(crate) filters: Vec<NotificationFilter>,
}

pub(crate) fn load_config() -> Result<Config> {
    fluidbg_plugin_sdk::load_yaml_config()
}
