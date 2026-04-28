use anyhow::{Context, Result, bail};
use serde_json::Value;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BgdStatus {
    pub phase: String,
    pub test_cases_observed: u64,
    pub test_cases_pending: u64,
    pub test_cases_passed: u64,
    pub test_cases_failed: u64,
    pub current_traffic_percent: u64,
}

impl BgdStatus {
    pub fn tracked_cases(&self) -> u64 {
        self.test_cases_observed + self.test_cases_pending
    }
}

pub fn bgd_status(document: &Value) -> BgdStatus {
    let status = document.get("status").unwrap_or(&Value::Null);
    BgdStatus {
        phase: string(status, "phase"),
        test_cases_observed: number(status, "testCasesObserved"),
        test_cases_pending: number(status, "testCasesPending"),
        test_cases_passed: number(status, "testCasesPassed"),
        test_cases_failed: number(status, "testCasesFailed"),
        current_traffic_percent: number(status, "currentTrafficPercent"),
    }
}

pub fn condition_status(document: &Value, condition_type: &str) -> Option<String> {
    document
        .pointer("/status/conditions")
        .and_then(Value::as_array)
        .and_then(|conditions| {
            conditions.iter().find_map(|condition| {
                if condition.get("type").and_then(Value::as_str) == Some(condition_type) {
                    condition
                        .get("status")
                        .and_then(Value::as_str)
                        .map(ToString::to_string)
                } else {
                    None
                }
            })
        })
}

pub fn ensure_no_drain_timeouts(document: &Value, bgd: &str) -> Result<()> {
    let timeouts: Vec<_> = document
        .pointer("/status/inceptionPointDrains")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|status| {
            status.get("phase").and_then(Value::as_str) == Some("timedOutMaybeSuccessful")
        })
        .collect();
    if timeouts.is_empty() {
        Ok(())
    } else {
        bail!("{bgd} has drain timeout statuses: {timeouts:?}")
    }
}

pub fn testcase_flags(document: &Value, test_id: &str) -> Result<TestcaseFlags> {
    let case = document
        .get(test_id)
        .with_context(|| format!("test case {test_id} not present in test container cases"))?;
    Ok(TestcaseFlags {
        status: string(case, "status"),
        output_message_seen: bool_value(case, "output_message_seen"),
        http_call_seen: bool_value(case, "http_call_seen"),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestcaseFlags {
    pub status: String,
    pub output_message_seen: bool,
    pub http_call_seen: bool,
}

pub fn number(document: &Value, key: &str) -> u64 {
    document.get(key).and_then(Value::as_u64).unwrap_or(0)
}

pub fn string(document: &Value, key: &str) -> String {
    document
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

pub fn bool_value(document: &Value, key: &str) -> bool {
    document.get(key).and_then(Value::as_bool).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn status_counts_tracked_cases() {
        let status = bgd_status(&json!({
            "status": {
                "phase": "Observing",
                "testCasesObserved": 2,
                "testCasesPending": 3,
                "testCasesPassed": 1,
                "currentTrafficPercent": 5
            }
        }));

        assert_eq!(status.phase, "Observing");
        assert_eq!(status.tracked_cases(), 5);
        assert_eq!(status.current_traffic_percent, 5);
    }

    #[test]
    fn condition_status_returns_matching_condition_only() {
        let document = json!({
            "status": {
                "conditions": [
                    {"type": "Ready", "status": "False"},
                    {"type": "Degraded", "status": "True"}
                ]
            }
        });

        assert_eq!(
            condition_status(&document, "Degraded").as_deref(),
            Some("True")
        );
        assert_eq!(condition_status(&document, "Progressing"), None);
    }

    #[test]
    fn detects_drain_timeout_statuses() {
        let document = json!({
            "status": {
                "inceptionPointDrains": [
                    {"phase": "drained"},
                    {"phase": "timedOutMaybeSuccessful"}
                ]
            }
        });

        assert!(ensure_no_drain_timeouts(&document, "bgd").is_err());
    }
}
