use anyhow::Result;
use chrono::Utc;
use reqwest::StatusCode;
use serde_json::Value;
use tokio::time::{Duration as TokioDuration, sleep};

use crate::auth::{AUTHORIZATION_HEADER, bearer_value};
use crate::models::{ObservationNotification, RegisterTestCaseRequest, TrafficRoute};

const DELIVERY_ATTEMPTS: usize = 10;
const DELIVERY_RETRY_DELAY: TokioDuration = TokioDuration::from_millis(250);

pub struct RegisterTestCaseArgs<'a> {
    pub testcase_registration_url: &'a str,
    pub blue_green_ref: &'a str,
    pub inception_point: &'a str,
    pub test_id: &'a str,
    pub test_container_url: &'a str,
    pub testcase_verify_path_template: Option<&'a str>,
    pub auth_token: Option<&'a str>,
}

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
    let url = format!("{}{}", test_container_url.trim_end_matches('/'), path);
    retry_status_request(|| client.post(&url).json(&notification)).await
}

pub async fn register_test_case(
    client: &reqwest::Client,
    args: RegisterTestCaseArgs<'_>,
) -> Result<()> {
    let verify_url = args.testcase_verify_path_template.map(|path| {
        format!(
            "{}{}",
            args.test_container_url.trim_end_matches('/'),
            render_path(path, args.test_id, args.inception_point)
        )
    });
    let request = RegisterTestCaseRequest {
        test_id: args.test_id.to_string(),
        blue_green_ref: args.blue_green_ref.to_string(),
        inception_point: args.inception_point.to_string(),
        triggered_at: Some(Utc::now()),
        timeout_seconds: Some(120),
        verify_url,
    };
    retry_status_request(|| {
        let mut builder = client.post(args.testcase_registration_url).json(&request);
        if let Some(auth_token) = args.auth_token {
            builder = builder.header(AUTHORIZATION_HEADER, bearer_value(auth_token));
        }
        builder
    })
    .await
}

async fn retry_status_request(mut request: impl FnMut() -> reqwest::RequestBuilder) -> Result<()> {
    let mut last_error: Option<anyhow::Error> = None;
    for attempt in 1..=DELIVERY_ATTEMPTS {
        match request().send().await {
            Ok(response) if response.status().is_success() => return Ok(()),
            Ok(response) if !retryable_status(response.status()) => {
                response.error_for_status()?;
                return Ok(());
            }
            Ok(response) => {
                let status = response.status();
                last_error = Some(anyhow::anyhow!("HTTP status {status}"));
            }
            Err(err) => {
                last_error = Some(err.into());
            }
        }
        if attempt < DELIVERY_ATTEMPTS {
            sleep(DELIVERY_RETRY_DELAY).await;
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("request failed")))
}

fn retryable_status(status: StatusCode) -> bool {
    status == StatusCode::REQUEST_TIMEOUT
        || status == StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
}
