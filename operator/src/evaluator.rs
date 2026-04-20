use crate::state_store::StateStore;
use std::sync::Arc;

pub async fn evaluate(
    store: &Arc<dyn StateStore>,
    bg_ref: &str,
    min_cases: i64,
    success_rate: f64,
) -> EvaluationResult {
    let counts = match store.counts(bg_ref).await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("failed to get counts: {}", e);
            return EvaluationResult::InsufficientData;
        }
    };

    let total = counts.passed + counts.failed + counts.timed_out;
    if total < min_cases {
        return EvaluationResult::InsufficientData;
    }

    let actual_rate = if total > 0 {
        counts.passed as f64 / total as f64
    } else {
        0.0
    };

    if actual_rate >= success_rate {
        EvaluationResult::Passed {
            success_rate: actual_rate,
            total,
            passed: counts.passed,
        }
    } else {
        EvaluationResult::Failed {
            success_rate: actual_rate,
            total,
            passed: counts.passed,
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum EvaluationResult {
    InsufficientData,
    Passed {
        success_rate: f64,
        total: i64,
        passed: i64,
    },
    Failed {
        success_rate: f64,
        total: i64,
        passed: i64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state_store::memory::MemoryStore;
    use crate::state_store::{InceptionTest, TestStatus};
    use chrono::{Duration, Utc};
    use std::sync::Arc;

    fn make_test(test_id: &str, bg_ref: &str, status: TestStatus) -> InceptionTest {
        InceptionTest {
            test_id: test_id.to_string(),
            blue_green_ref: bg_ref.to_string(),
            triggered_at: Utc::now(),
            trigger_inception_point: "test-point".to_string(),
            timeout: Duration::seconds(60),
            status,
            verdict: None,
        }
    }

    #[tokio::test]
    async fn insufficient_data_when_below_min_cases() {
        let store: Arc<dyn StateStore> = Arc::new(MemoryStore::new());
        let run = make_test("t1", "bg", TestStatus::Triggered);
        store.register(run).await.unwrap();
        store.set_verdict("t1", true).await.unwrap();

        let result = evaluate(&store, "bg", 100, 0.98).await;
        assert_eq!(result, EvaluationResult::InsufficientData);
    }

    #[tokio::test]
    async fn passed_when_rate_meets_threshold() {
        let store: Arc<dyn StateStore> = Arc::new(MemoryStore::new());
        for i in 0..100 {
            let id = format!("t-{}", i);
            store
                .register(make_test(&id, "bg", TestStatus::Triggered))
                .await
                .unwrap();
            store.set_verdict(&id, true).await.unwrap();
        }

        let result = evaluate(&store, "bg", 100, 0.98).await;
        match result {
            EvaluationResult::Passed {
                success_rate,
                total,
                ..
            } => {
                assert!(success_rate >= 0.98);
                assert_eq!(total, 100);
            }
            _ => panic!("expected Passed, got {:?}", result),
        }
    }

    #[tokio::test]
    async fn failed_when_rate_below_threshold() {
        let store: Arc<dyn StateStore> = Arc::new(MemoryStore::new());
        for i in 0..100 {
            let id = format!("t-{}", i);
            store
                .register(make_test(&id, "bg", TestStatus::Triggered))
                .await
                .unwrap();
            if i < 95 {
                store.set_verdict(&id, true).await.unwrap();
            } else {
                store.set_verdict(&id, false).await.unwrap();
            }
        }

        let result = evaluate(&store, "bg", 100, 0.98).await;
        match result {
            EvaluationResult::Failed {
                success_rate,
                total,
                ..
            } => {
                assert!(success_rate < 0.98);
                assert_eq!(total, 100);
            }
            _ => panic!("expected Failed, got {:?}", result),
        }
    }

    #[tokio::test]
    async fn timed_out_counts_against_success_rate() {
        let store: Arc<dyn StateStore> = Arc::new(MemoryStore::new());
        for i in 0..100 {
            let id = format!("t-{}", i);
            store
                .register(make_test(&id, "bg", TestStatus::Triggered))
                .await
                .unwrap();
            if i < 96 {
                store.set_verdict(&id, true).await.unwrap();
            } else {
                store.mark_timed_out(&id).await.unwrap();
            }
        }

        let result = evaluate(&store, "bg", 100, 0.98).await;
        match result {
            EvaluationResult::Failed { success_rate, .. } => {
                assert!(success_rate < 0.98);
            }
            _ => panic!("expected Failed, got {:?}", result),
        }
    }
}
