use std::collections::HashMap;

use async_trait::async_trait;
use chrono::Utc;
use tokio::sync::RwLock;

use super::{Counts, Result, StateStore, StoreError, TestCaseRecord, TestStatus, VerificationMode};

pub struct MemoryStore {
    data: RwLock<HashMap<String, TestCaseRecord>>,
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryStore {
    pub fn new() -> Self {
        Self {
            data: RwLock::new(HashMap::new()),
        }
    }
}

#[async_trait]
impl StateStore for MemoryStore {
    async fn register(&self, run: TestCaseRecord) -> Result<()> {
        let mut data = self.data.write().await;
        data.insert(run.test_id.clone(), run);
        Ok(())
    }

    async fn get(&self, test_id: &str) -> Result<Option<TestCaseRecord>> {
        let data = self.data.read().await;
        Ok(data.get(test_id).cloned())
    }

    async fn set_verdict(
        &self,
        test_id: &str,
        passed: bool,
        failure_message: Option<String>,
    ) -> Result<()> {
        let mut data = self.data.write().await;
        if let Some(run) = data.get_mut(test_id) {
            run.verdict = Some(passed);
            run.failure_message = failure_message;
            run.status = if passed {
                TestStatus::Passed
            } else {
                TestStatus::Failed
            };
            Ok(())
        } else {
            Err(StoreError::NotFound(test_id.to_string()))
        }
    }

    async fn mark_timed_out(&self, test_id: &str) -> Result<()> {
        let mut data = self.data.write().await;
        if let Some(run) = data.get_mut(test_id) {
            run.status = TestStatus::TimedOut;
            run.verdict = None;
            Ok(())
        } else {
            Err(StoreError::NotFound(test_id.to_string()))
        }
    }

    async fn decrement_retries(&self, test_id: &str) -> Result<Option<i32>> {
        let mut data = self.data.write().await;
        if let Some(run) = data.get_mut(test_id) {
            if run.retries_remaining > 0 {
                run.retries_remaining -= 1;
                return Ok(Some(run.retries_remaining));
            }
            return Ok(None);
        }
        Err(StoreError::NotFound(test_id.to_string()))
    }

    async fn list_pending(&self) -> Result<Vec<TestCaseRecord>> {
        let data = self.data.read().await;
        Ok(data.values().filter(|r| r.is_pending()).cloned().collect())
    }

    async fn counts(&self, bg: &str) -> Result<Counts> {
        let data = self.data.read().await;
        Ok(counts_for_runs(
            data.values().filter(|r| r.blue_green_ref == bg).cloned().collect(),
        ))
    }

    async fn counts_for_mode(&self, bg: &str, mode: VerificationMode) -> Result<Counts> {
        let data = self.data.read().await;
        Ok(counts_for_runs(
            data.values()
                .filter(|r| r.blue_green_ref == bg && r.verification_mode == mode)
                .cloned()
                .collect(),
        ))
    }

    async fn latest_failure_message(&self, bg: &str) -> Result<Option<String>> {
        let data = self.data.read().await;
        Ok(data
            .values()
            .filter(|run| run.blue_green_ref == bg)
            .filter_map(|run| run.failure_message.clone())
            .last())
    }

    async fn cleanup_expired(&self) -> Result<usize> {
        let mut data = self.data.write().await;
        let now = Utc::now();
        let before = data.len();
        data.retain(|_, run| {
            if run.is_pending() && run.expires_at() < now {
                return false;
            }
            true
        });
        Ok(before - data.len())
    }
}

fn counts_for_runs(runs: Vec<TestCaseRecord>) -> Counts {
    let mut counts = Counts::default();
    for run in runs {
        match run.status {
            TestStatus::Passed => counts.passed += 1,
            TestStatus::Failed => counts.failed += 1,
            TestStatus::TimedOut => counts.timed_out += 1,
            TestStatus::Triggered | TestStatus::Observing => counts.pending += 1,
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};

    use super::*;

    fn make_test(test_id: &str, bg_ref: &str) -> TestCaseRecord {
        TestCaseRecord {
            test_id: test_id.to_string(),
            blue_green_ref: bg_ref.to_string(),
            triggered_at: Utc::now(),
            source_inception_point: "test-point".to_string(),
            timeout: Duration::seconds(60),
            status: TestStatus::Triggered,
            verdict: None,
            verification_mode: VerificationMode::Data,
            verify_url: "http://test/result".to_string(),
            retries_remaining: 0,
            failure_message: None,
        }
    }

    #[tokio::test]
    async fn register_then_get_returns_same_record() {
        let store = MemoryStore::new();
        let run = make_test("ORD-1", "order-processor");
        store.register(run.clone()).await.unwrap();
        let got = store.get("ORD-1").await.unwrap().unwrap();
        assert_eq!(got.test_id, "ORD-1");
        assert_eq!(got.blue_green_ref, "order-processor");
    }

    #[tokio::test]
    async fn get_missing_returns_none() {
        let store = MemoryStore::new();
        assert!(store.get("nonexistent").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn set_verdict_passed() {
        let store = MemoryStore::new();
        store.register(make_test("ORD-2", "bg")).await.unwrap();
        store.set_verdict("ORD-2", true, None).await.unwrap();
        let got = store.get("ORD-2").await.unwrap().unwrap();
        assert_eq!(got.status, TestStatus::Passed);
        assert_eq!(got.verdict, Some(true));
    }

    #[tokio::test]
    async fn set_verdict_failed() {
        let store = MemoryStore::new();
        store.register(make_test("ORD-3", "bg")).await.unwrap();
        store
            .set_verdict("ORD-3", false, Some("boom".to_string()))
            .await
            .unwrap();
        let got = store.get("ORD-3").await.unwrap().unwrap();
        assert_eq!(got.status, TestStatus::Failed);
        assert_eq!(got.verdict, Some(false));
    }

    #[tokio::test]
    async fn mark_timed_out() {
        let store = MemoryStore::new();
        store.register(make_test("ORD-4", "bg")).await.unwrap();
        store.mark_timed_out("ORD-4").await.unwrap();
        let got = store.get("ORD-4").await.unwrap().unwrap();
        assert_eq!(got.status, TestStatus::TimedOut);
        assert_eq!(got.verdict, None);
    }

    #[tokio::test]
    async fn list_pending_returns_only_pending() {
        let store = MemoryStore::new();
        store.register(make_test("ORD-10", "bg")).await.unwrap();

        let mut observing = make_test("ORD-11", "bg");
        observing.status = TestStatus::Observing;
        store.register(observing).await.unwrap();

        let mut passed = make_test("ORD-12", "bg");
        passed.status = TestStatus::Passed;
        store.register(passed).await.unwrap();

        let pending = store.list_pending().await.unwrap();
        assert_eq!(pending.len(), 2);
    }

    #[tokio::test]
    async fn counts_matches_verdicts() {
        let store = MemoryStore::new();
        store.register(make_test("ORD-20", "bg1")).await.unwrap();
        store.register(make_test("ORD-21", "bg1")).await.unwrap();
        store.register(make_test("ORD-22", "bg1")).await.unwrap();
        store.register(make_test("ORD-23", "bg2")).await.unwrap();

        store.set_verdict("ORD-20", true, None).await.unwrap();
        store.set_verdict("ORD-21", false, None).await.unwrap();
        store.mark_timed_out("ORD-22").await.unwrap();

        let counts = store.counts("bg1").await.unwrap();
        assert_eq!(counts.passed, 1);
        assert_eq!(counts.failed, 1);
        assert_eq!(counts.timed_out, 1);
        assert_eq!(counts.pending, 0);
    }

    #[tokio::test]
    async fn cleanup_expired_removes_only_expired_pending() {
        let store = MemoryStore::new();

        let mut expired = make_test("EXP-1", "bg");
        expired.triggered_at = Utc::now() - Duration::seconds(120);
        expired.timeout = Duration::seconds(10);
        store.register(expired).await.unwrap();

        store.register(make_test("LIVE-1", "bg")).await.unwrap();

        let mut passed_expired = make_test("EXP-2", "bg");
        passed_expired.triggered_at = Utc::now() - Duration::seconds(120);
        passed_expired.timeout = Duration::seconds(10);
        passed_expired.status = TestStatus::Passed;
        store.register(passed_expired).await.unwrap();

        let removed = store.cleanup_expired().await.unwrap();
        assert_eq!(removed, 1);
        assert!(store.get("EXP-1").await.unwrap().is_none());
        assert!(store.get("LIVE-1").await.unwrap().is_some());
        assert!(store.get("EXP-2").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn concurrent_register_and_verdict() {
        let store = std::sync::Arc::new(MemoryStore::new());
        let mut handles = vec![];

        for i in 0..50 {
            let s = store.clone();
            handles.push(tokio::spawn(async move {
                let id = format!("CONC-{}", i);
                s.register(make_test(&id, "bg")).await.unwrap();
                s.set_verdict(&id, i % 2 == 0, None).await.unwrap();
            }));
        }

        for h in handles {
            h.await.unwrap();
        }

        let counts = store.counts("bg").await.unwrap();
        assert_eq!(counts.passed + counts.failed, 50);
    }
}
