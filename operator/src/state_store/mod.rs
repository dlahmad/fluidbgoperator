pub mod memory;
pub mod postgres;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use thiserror::Error;

use crate::crd::state_store::StateStore as StateStoreCRD;

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

#[derive(Clone, Debug)]
pub struct InceptionTest {
    pub test_id: String,
    pub blue_green_ref: String,
    pub triggered_at: DateTime<Utc>,
    pub trigger_inception_point: String,
    pub timeout: Duration,
    pub status: TestStatus,
    pub verdict: Option<bool>,
}

impl InceptionTest {
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
    async fn register(&self, run: InceptionTest) -> Result<()>;
    async fn get(&self, test_id: &str) -> Result<Option<InceptionTest>>;
    async fn set_verdict(&self, test_id: &str, passed: bool) -> Result<()>;
    async fn mark_timed_out(&self, test_id: &str) -> Result<()>;
    async fn list_pending(&self) -> Result<Vec<InceptionTest>>;
    async fn counts(&self, bg: &str) -> Result<Counts>;
    async fn cleanup_expired(&self) -> Result<usize>;
}

pub async fn state_store_from_crd(crd: &StateStoreCRD) -> Result<Arc<dyn StateStore>> {
    use crate::crd::state_store::StateStoreType;
    match &crd.spec.store_type {
        StateStoreType::Memory => Ok(Arc::new(memory::MemoryStore::new())),
        StateStoreType::Postgres => {
            let cfg = crd
                .spec
                .postgres
                .as_ref()
                .ok_or_else(|| StoreError::Other("postgres config required".to_string()))?;
            let store = postgres::PostgresStore::new(&cfg.url, &cfg.table_name).await?;
            store.migrate().await?;
            Ok(Arc::new(store))
        }
        StateStoreType::Redis => Err(StoreError::Other(
            "redis state store not yet implemented".to_string(),
        )),
    }
}

use std::sync::Arc;
