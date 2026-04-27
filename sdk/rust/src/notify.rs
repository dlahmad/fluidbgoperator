use anyhow::Result;
use chrono::Utc;
use serde_json::Value;

use crate::auth::{AUTHORIZATION_HEADER, bearer_value};
use crate::models::{ObservationNotification, RegisterTestCaseRequest, TrafficRoute};

pub fn render_path(template: &str, test_id: &str, inception_point: &str) -> String {
    template
        .replace("{testId}", test_id)
        .replace("{inceptionPoint}", inception_point)
}

pub async fn notify_observer(
    client: &reqwest::Client,
    test_container_url: &str,
    notify_path: &str,
    test_id: &str,
    inception_point: &str,
    payload: &Value,
    route: TrafficRoute,
) -> Result<()> {
    let path = render_path(notify_path, test_id, inception_point);
    let notification = ObservationNotification {
        test_id,
        inception_point,
        route: route.as_str(),
        payload,
    };
    client
        .post(format!(
            "{}{}",
            test_container_url.trim_end_matches('/'),
            path
        ))
        .json(&notification)
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}

pub async fn register_test_case(
    client: &reqwest::Client,
    testcase_registration_url: &str,
    blue_green_ref: &str,
    inception_point: &str,
    test_id: &str,
    test_container_url: &str,
    testcase_verify_path_template: Option<&str>,
    auth_token: Option<&str>,
) -> Result<()> {
    let verify_url = testcase_verify_path_template.map(|path| {
        format!(
            "{}{}",
            test_container_url.trim_end_matches('/'),
            render_path(path, test_id, inception_point)
        )
    });
    let request = RegisterTestCaseRequest {
        test_id: test_id.to_string(),
        blue_green_ref: blue_green_ref.to_string(),
        inception_point: inception_point.to_string(),
        triggered_at: Some(Utc::now()),
        timeout_seconds: Some(120),
        verify_url,
    };
    let mut builder = client.post(testcase_registration_url).json(&request);
    if let Some(auth_token) = auth_token {
        builder = builder.header(AUTHORIZATION_HEADER, bearer_value(auth_token));
    }
    builder.send().await?.error_for_status()?;
    Ok(())
}
