use anyhow::Result;
use serde_json::Value;

use crate::config::{active_roles, has_role};
use crate::models::{PluginRole, TrafficRoute};
use crate::notify::{notify_observer, register_test_case};

#[derive(Clone)]
pub struct PluginRuntime {
    client: reqwest::Client,
    roles: Vec<PluginRole>,
    mode: String,
    testcase_registration_url: String,
    test_container_url: String,
    testcase_verify_path_template: Option<String>,
    inception_point: String,
    blue_green_ref: String,
}

impl PluginRuntime {
    pub fn from_env() -> Self {
        Self {
            client: reqwest::Client::new(),
            roles: active_roles(),
            mode: std::env::var("FLUIDBG_MODE")
                .unwrap_or_else(|_| "passthrough-duplicate".to_string()),
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
        }
    }

    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    pub fn roles(&self) -> &[PluginRole] {
        &self.roles
    }

    pub fn mode(&self) -> &str {
        &self.mode
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

    pub fn has_role(&self, role: PluginRole) -> bool {
        has_role(&self.roles, role)
    }

    pub fn observes(&self) -> bool {
        self.has_role(PluginRole::Observer)
            || self.mode == "trigger"
            || self.mode == "passthrough-duplicate"
    }

    pub fn mocks(&self) -> bool {
        self.has_role(PluginRole::Mock) || self.mode == "reroute-mock"
    }

    pub async fn register_test_case(&self, test_id: &str) -> Result<()> {
        register_test_case(
            &self.client,
            &self.testcase_registration_url,
            &self.blue_green_ref,
            &self.inception_point,
            test_id,
            &self.test_container_url,
            self.testcase_verify_path_template.as_deref(),
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
            &self.test_container_url,
            notify_path,
            test_id,
            &self.inception_point,
            payload,
            route,
        )
        .await
    }
}
