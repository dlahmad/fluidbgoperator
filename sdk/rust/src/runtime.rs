use anyhow::Result;
use serde_json::Value;

use crate::auth::auth_token_from_env;
use crate::config::{active_roles, has_role};
use crate::models::{PluginRole, TrafficRoute};
use crate::notify::{
    NotifyObserverArgs, RegisterTestCaseArgs, notify_observer, register_test_case,
};

#[derive(Clone)]
pub struct PluginInceptorRuntime {
    client: reqwest::Client,
    roles: Vec<PluginRole>,
    testcase_registration_url: String,
    test_container_url: String,
    testcase_verify_path_template: Option<String>,
    inception_point: String,
    blue_green_ref: String,
    auth_token: Option<String>,
}

impl PluginInceptorRuntime {
    pub fn from_env() -> Self {
        Self {
            client: runtime_http_client().unwrap_or_else(|err| {
                tracing::warn!(
                    "failed to build configured plugin runtime HTTP client, using defaults: {}",
                    err
                );
                reqwest::Client::new()
            }),
            roles: active_roles(),
            testcase_registration_url: std::env::var("FLUIDBG_TESTCASE_REGISTRATION_URL")
                .unwrap_or_else(|_| "http://localhost:8090/testcases".to_string()),
            test_container_url: std::env::var("FLUIDBG_TEST_CONTAINER_URL")
                .unwrap_or_else(|_| "http://localhost:8080".to_string()),
            testcase_verify_path_template: std::env::var("FLUIDBG_TESTCASE_VERIFY_PATH_TEMPLATE")
                .ok(),
            inception_point: std::env::var("FLUIDBG_INCEPTION_POINT")
                .unwrap_or_else(|_| "unknown".to_string()),
            blue_green_ref: std::env::var("FLUIDBG_BLUE_GREEN_REF")
                .unwrap_or_else(|_| "unknown".to_string()),
            auth_token: auth_token_from_env(),
        }
    }

    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    pub fn roles(&self) -> &[PluginRole] {
        &self.roles
    }

    pub fn test_container_url(&self) -> &str {
        &self.test_container_url
    }

    pub fn inception_point(&self) -> &str {
        &self.inception_point
    }

    pub fn blue_green_ref(&self) -> &str {
        &self.blue_green_ref
    }

    pub fn auth_token(&self) -> Option<&str> {
        self.auth_token.as_deref()
    }

    pub fn has_role(&self, role: PluginRole) -> bool {
        has_role(&self.roles, role)
    }

    pub async fn register_test_case(&self, test_id: &str) -> Result<()> {
        register_test_case(
            &self.client,
            RegisterTestCaseArgs {
                testcase_registration_url: &self.testcase_registration_url,
                blue_green_ref: &self.blue_green_ref,
                inception_point: &self.inception_point,
                test_id,
                test_container_url: &self.test_container_url,
                testcase_verify_path_template: self.testcase_verify_path_template.as_deref(),
                auth_token: self.auth_token.as_deref(),
            },
        )
        .await
    }

    pub async fn notify_observer(
        &self,
        notify_path: &str,
        test_id: &str,
        payload: &Value,
        route: TrafficRoute,
    ) -> Result<()> {
        notify_observer(
            &self.client,
            NotifyObserverArgs {
                test_container_url: &self.test_container_url,
                notify_path,
                test_id,
                inception_point: &self.inception_point,
                payload,
                route,
                auth_token: self.auth_token.as_deref(),
            },
        )
        .await
    }
}

pub type PluginRuntime = PluginInceptorRuntime;

fn runtime_http_client() -> anyhow::Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder();
    if crate::config::env_flag("FLUIDBG_OPERATOR_INSECURE_SKIP_VERIFY") {
        builder = builder.danger_accept_invalid_certs(true);
    }
    if let Some(path) = crate::config::optional_env("FLUIDBG_OPERATOR_CA_CERT_PATH") {
        let pem = std::fs::read(&path).map_err(|err| {
            anyhow::anyhow!("failed to read operator CA certificate {path}: {err}")
        })?;
        let cert = reqwest::Certificate::from_pem(&pem).map_err(|err| {
            anyhow::anyhow!("failed to parse operator CA certificate {path}: {err}")
        })?;
        builder = builder.add_root_certificate(cert);
    }
    Ok(builder.build()?)
}
