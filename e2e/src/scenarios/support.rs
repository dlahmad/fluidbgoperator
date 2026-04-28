use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail};
use serde_json::Value;

use crate::command;
use crate::harness::E2eHarness;
use crate::status::{bgd_status, condition_status, testcase_flags};

pub async fn wait_for_tracked_cases(
    harness: &E2eHarness,
    bgd: &str,
    min_tracked: u64,
    min_passed: u64,
    attempts: u32,
) -> Result<()> {
    for _ in 1..=attempts {
        let status = bgd_status(
            &harness
                .kube
                .bgd_json(bgd, &harness.config.namespace)
                .await?,
        );
        if status.tracked_cases() >= min_tracked && status.test_cases_passed >= min_passed {
            return Ok(());
        }
        if status.phase == "RolledBack" {
            bail!("{bgd} rolled back while waiting for observed test cases");
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    bail!("expected at least {min_tracked} tracked and {min_passed} passed test cases for {bgd}")
}

pub async fn wait_for_terminal_phase(
    harness: &E2eHarness,
    bgd: &str,
    expected: &str,
    attempts: u32,
) -> Result<()> {
    for _ in 1..=attempts {
        let status = bgd_status(
            &harness
                .kube
                .bgd_json(bgd, &harness.config.namespace)
                .await?,
        );
        if status.phase == expected {
            return Ok(());
        }
        if status.phase == "RolledBack" && expected != "RolledBack" {
            bail!("{bgd} rolled back");
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    bail!("{bgd} did not reach phase {expected}")
}

pub async fn wait_for_queue_json_field(
    harness: &mut E2eHarness,
    queue: &str,
    field: &str,
    expected: &str,
    attempts: u32,
) -> Result<()> {
    for _ in 1..=attempts {
        if harness
            .rabbitmq
            .queue_contains_json_field(queue, field, expected)
            .await
            .unwrap_or(false)
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    bail!("expected queue {queue} to contain {field}={expected}")
}

pub async fn wait_for_bgd_rollout_generation(
    harness: &E2eHarness,
    bgd: &str,
    attempts: u32,
) -> Result<i64> {
    for _ in 1..=attempts {
        let document = harness
            .kube
            .bgd_json(bgd, &harness.config.namespace)
            .await?;
        let generation = document
            .pointer("/metadata/generation")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        let rollout_generation = document
            .pointer("/status/rolloutGeneration")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        if generation > 0 && generation == rollout_generation {
            return Ok(generation);
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    bail!("{bgd} did not start reconciling the latest generation")
}

pub fn http_case_flags(
    namespace: &str,
    test_deployment: &str,
    test_id: &str,
) -> Result<crate::status::TestcaseFlags> {
    let output = command::output(
        "kubectl",
        [
            "exec",
            "-n",
            namespace,
            &format!("deploy/{test_deployment}"),
            "--",
            "python",
            "-c",
            &format!(
                "import json,urllib.request; print(json.dumps(json.load(urllib.request.urlopen('http://localhost:8080/cases')).get({test_id:?}, {{}})))"
            ),
        ],
    )?;
    let document: Value = serde_json::from_str(&format!(r#"{{"{test_id}":{output}}}"#))?;
    testcase_flags(&document, test_id)
}

pub async fn wait_http_case_verified(
    namespace: &str,
    test_deployment: &str,
    test_id: &str,
    attempts: u64,
) -> Result<()> {
    for _ in 1..=attempts {
        if let Ok(flags) = http_case_flags(namespace, test_deployment, test_id) {
            if flags.status == "failed" {
                bail!("HTTP proxy case {test_id} failed in verifier");
            }
            if flags.status == "passed" && flags.output_message_seen && flags.http_call_seen {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    bail!("expected HTTP proxy case {test_id} to observe both the HTTP call and output message")
}

pub fn assert_condition(
    document: &Value,
    bgd: &str,
    condition: &str,
    expected: &str,
) -> Result<()> {
    let actual = condition_status(document, condition).unwrap_or_default();
    if actual == expected {
        Ok(())
    } else {
        bail!("expected bluegreendeployment/{bgd} condition {condition}={expected}, got {actual}")
    }
}

pub fn unique_token(prefix: &str) -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    format!("{prefix}-{millis}")
}
