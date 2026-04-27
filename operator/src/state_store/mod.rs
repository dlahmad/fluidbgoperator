pub mod azure_identity;
pub mod cosmos;
pub mod memory;
pub mod postgres;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use std::collections::BTreeSet;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("postgres error: {0}")]
    Postgres(#[from] sqlx::Error),
    #[error("concurrent modification: {0}")]
    Conflict(String),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

#[derive(Clone, Debug, PartialEq)]
pub enum TestStatus {
    Triggered,
    Observing,
    Passed,
    Failed,
    TimedOut,
}

#[derive(Clone, Debug, PartialEq)]
pub enum VerificationMode {
    Data,
    Custom,
}

#[derive(Clone, Debug)]
pub struct TestCaseRecord {
    pub test_id: String,
    pub blue_green_ref: String,
    pub triggered_at: DateTime<Utc>,
    pub source_inception_point: String,
    pub timeout: Duration,
    pub status: TestStatus,
    pub verdict: Option<bool>,
    pub verification_mode: VerificationMode,
    pub verify_url: String,
    pub retries_remaining: i32,
    pub failure_message: Option<String>,
}

impl TestCaseRecord {
    pub fn expires_at(&self) -> DateTime<Utc> {
        self.triggered_at + self.timeout
    }

    pub fn is_pending(&self) -> bool {
        matches!(self.status, TestStatus::Triggered | TestStatus::Observing)
    }

    pub fn is_finalized(&self) -> bool {
        matches!(
            self.status,
            TestStatus::Passed | TestStatus::Failed | TestStatus::TimedOut
        )
    }
}

#[derive(Clone, Debug, Default)]
pub struct Counts {
    pub passed: i64,
    pub failed: i64,
    pub timed_out: i64,
    pub pending: i64,
}

#[async_trait]
pub trait StateStore: Send + Sync {
    async fn register(&self, run: TestCaseRecord) -> Result<()>;
    async fn get(&self, blue_green_ref: &str, test_id: &str) -> Result<Option<TestCaseRecord>>;
    async fn set_verdict(
        &self,
        blue_green_ref: &str,
        test_id: &str,
        passed: bool,
        failure_message: Option<String>,
    ) -> Result<()>;
    async fn mark_timed_out(&self, blue_green_ref: &str, test_id: &str) -> Result<()>;
    async fn decrement_retries(&self, blue_green_ref: &str, test_id: &str) -> Result<Option<i32>>;
    async fn list_pending(&self) -> Result<Vec<TestCaseRecord>>;
    async fn list_blue_green_refs(&self) -> Result<BTreeSet<String>>;
    async fn counts(&self, bg: &str) -> Result<Counts>;
    async fn counts_for_mode(&self, bg: &str, mode: VerificationMode) -> Result<Counts>;
    async fn latest_failure_message(&self, bg: &str) -> Result<Option<String>>;
    async fn cleanup_blue_green(&self, bg: &str) -> Result<usize>;
    async fn cleanup_expired(&self) -> Result<usize>;
}
