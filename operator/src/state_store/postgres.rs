use async_trait::async_trait;
use chrono::Duration;

use super::{Counts, Result, StateStore, StoreError, TestCaseRecord, TestStatus, VerificationMode};

pub struct PostgresStore {
    pool: sqlx::PgPool,
    table_name: String,
}

impl PostgresStore {
    pub async fn new(url: &str, table_name: &str) -> Result<Self> {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(5)
            .connect(url)
            .await
            .map_err(StoreError::Postgres)?;
        Ok(Self {
            pool,
            table_name: table_name.to_string(),
        })
    }

    pub fn new_lazy(url: &str, table_name: &str) -> Self {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(5)
            .connect_lazy(url)
            .expect("failed to create postgres pool");
        Self {
            pool,
            table_name: table_name.to_string(),
        }
    }

    pub async fn migrate(&self) -> Result<()> {
        let create_table = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {table} (
                test_id TEXT NOT NULL,
                blue_green_ref TEXT NOT NULL,
                triggered_at TIMESTAMPTZ NOT NULL,
                trigger_inception_point TEXT NOT NULL,
                timeout_seconds INTEGER NOT NULL,
                status TEXT NOT NULL,
                verdict BOOLEAN,
                verification_mode TEXT NOT NULL DEFAULT 'Data',
                verify_url TEXT NOT NULL DEFAULT '',
                retries_remaining INTEGER NOT NULL DEFAULT 0,
                failure_message TEXT,
                expires_at TIMESTAMPTZ NOT NULL,
                PRIMARY KEY (blue_green_ref, test_id)
            )"#,
            table = self.table_name
        );
        let create_index = format!(
            r#"CREATE INDEX IF NOT EXISTS idx_{table}_expires ON {table} (expires_at)
               WHERE status IN ('Triggered', 'Observing')"#,
            table = self.table_name
        );
        sqlx::query(&create_table)
            .execute(&self.pool)
            .await
            .map_err(StoreError::Postgres)?;
        sqlx::query(&create_index)
            .execute(&self.pool)
            .await
            .map_err(StoreError::Postgres)?;
        Ok(())
    }
}

fn row_to_test(row: &sqlx::postgres::PgRow) -> TestCaseRecord {
    use sqlx::Row;
    let status_str: String = row.get("status");
    let status = match status_str.as_str() {
        "Triggered" => TestStatus::Triggered,
        "Observing" => TestStatus::Observing,
        "Passed" => TestStatus::Passed,
        "Failed" => TestStatus::Failed,
        "TimedOut" => TestStatus::TimedOut,
        _ => TestStatus::Triggered,
    };
    TestCaseRecord {
        test_id: row.get("test_id"),
        blue_green_ref: row.get("blue_green_ref"),
        triggered_at: row.get("triggered_at"),
        source_inception_point: row.get("trigger_inception_point"),
        timeout: Duration::seconds(row.get::<i32, _>("timeout_seconds") as i64),
        status,
        verdict: row.get("verdict"),
        verification_mode: match row.get::<String, _>("verification_mode").as_str() {
            "Custom" => VerificationMode::Custom,
            _ => VerificationMode::Data,
        },
        verify_url: row.get("verify_url"),
        retries_remaining: row.get::<i32, _>("retries_remaining"),
        failure_message: row.get("failure_message"),
    }
}

#[async_trait]
impl StateStore for PostgresStore {
    async fn register(&self, run: TestCaseRecord) -> Result<()> {
        let status_str = match run.status {
            TestStatus::Triggered => "Triggered",
            TestStatus::Observing => "Observing",
            TestStatus::Passed => "Passed",
            TestStatus::Failed => "Failed",
            TestStatus::TimedOut => "TimedOut",
        };
        let query = format!(
            r#"INSERT INTO {table} (test_id, blue_green_ref, triggered_at, trigger_inception_point, timeout_seconds, status, verdict, verification_mode, verify_url, retries_remaining, failure_message, expires_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)"#,
            table = self.table_name
        );
        sqlx::query(&query)
            .bind(&run.test_id)
            .bind(&run.blue_green_ref)
            .bind(run.triggered_at)
            .bind(&run.source_inception_point)
            .bind(run.timeout.num_seconds() as i32)
            .bind(status_str)
            .bind(run.verdict)
            .bind(match run.verification_mode {
                VerificationMode::Data => "Data",
                VerificationMode::Custom => "Custom",
            })
            .bind(&run.verify_url)
            .bind(run.retries_remaining)
            .bind(&run.failure_message)
            .bind(run.expires_at())
            .execute(&self.pool)
            .await
            .map_err(StoreError::Postgres)?;
        Ok(())
    }

    async fn get(&self, test_id: &str) -> Result<Option<TestCaseRecord>> {
        let query = format!("SELECT * FROM {} WHERE test_id = $1", self.table_name);
        let row = sqlx::query(&query)
            .bind(test_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Postgres)?;
        Ok(row.as_ref().map(row_to_test))
    }

    async fn set_verdict(
        &self,
        test_id: &str,
        passed: bool,
        failure_message: Option<String>,
    ) -> Result<()> {
        let status_str = if passed { "Passed" } else { "Failed" };
        let query = format!(
            "UPDATE {} SET status = $1, verdict = $2, failure_message = $3 WHERE test_id = $4 AND status IN ('Triggered', 'Observing')",
            self.table_name
        );
        let result = sqlx::query(&query)
            .bind(status_str)
            .bind(passed)
            .bind(failure_message)
            .bind(test_id)
            .execute(&self.pool)
            .await
            .map_err(StoreError::Postgres)?;
        if result.rows_affected() == 0 {
            return Err(StoreError::NotFound(test_id.to_string()));
        }
        Ok(())
    }

    async fn mark_timed_out(&self, test_id: &str) -> Result<()> {
        let query = format!(
            "UPDATE {} SET status = 'TimedOut', verdict = NULL WHERE test_id = $1 AND status IN ('Triggered', 'Observing')",
            self.table_name
        );
        let result = sqlx::query(&query)
            .bind(test_id)
            .execute(&self.pool)
            .await
            .map_err(StoreError::Postgres)?;
        if result.rows_affected() == 0 {
            return Err(StoreError::NotFound(test_id.to_string()));
        }
        Ok(())
    }

    async fn decrement_retries(&self, test_id: &str) -> Result<Option<i32>> {
        let query = format!(
            "UPDATE {} SET retries_remaining = retries_remaining - 1 WHERE test_id = $1 AND retries_remaining > 0 RETURNING retries_remaining",
            self.table_name
        );
        let row = sqlx::query(&query)
            .bind(test_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Postgres)?;
        use sqlx::Row;
        Ok(row.map(|row| row.get::<i32, _>("retries_remaining")))
    }

    async fn list_pending(&self) -> Result<Vec<TestCaseRecord>> {
        let query = format!(
            "SELECT * FROM {} WHERE status IN ('Triggered', 'Observing')",
            self.table_name
        );
        let rows = sqlx::query(&query)
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Postgres)?;
        Ok(rows.iter().map(row_to_test).collect())
    }

    async fn counts(&self, bg: &str) -> Result<Counts> {
        self.counts_query("blue_green_ref = $1", vec![bg.to_string()])
            .await
    }

    async fn counts_for_mode(&self, bg: &str, mode: VerificationMode) -> Result<Counts> {
        let mode = match mode {
            VerificationMode::Data => "Data".to_string(),
            VerificationMode::Custom => "Custom".to_string(),
        };
        self.counts_query(
            "blue_green_ref = $1 AND verification_mode = $2",
            vec![bg.to_string(), mode],
        )
        .await
    }

    async fn latest_failure_message(&self, bg: &str) -> Result<Option<String>> {
        let query = format!(
            "SELECT failure_message FROM {} WHERE blue_green_ref = $1 AND failure_message IS NOT NULL ORDER BY triggered_at DESC LIMIT 1",
            self.table_name
        );
        let row = sqlx::query(&query)
            .bind(bg)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Postgres)?;
        use sqlx::Row;
        Ok(row.map(|row| row.get::<String, _>("failure_message")))
    }

    async fn cleanup_expired(&self) -> Result<usize> {
        let query = format!(
            "DELETE FROM {} WHERE expires_at < NOW() AND status IN ('Triggered', 'Observing')",
            self.table_name
        );
        let result = sqlx::query(&query)
            .execute(&self.pool)
            .await
            .map_err(StoreError::Postgres)?;
        Ok(result.rows_affected() as usize)
    }
}

impl PostgresStore {
    async fn counts_query(&self, predicate: &str, binds: Vec<String>) -> Result<Counts> {
        let query = format!(
            r#"SELECT
                COUNT(*) FILTER (WHERE status = 'Passed') as passed,
                COUNT(*) FILTER (WHERE status = 'Failed') as failed,
                COUNT(*) FILTER (WHERE status = 'TimedOut') as timed_out,
                COUNT(*) FILTER (WHERE status IN ('Triggered', 'Observing')) as pending
               FROM {} WHERE {}"#,
            self.table_name, predicate
        );
        let mut q = sqlx::query(&query);
        for bind in binds {
            q = q.bind(bind);
        }
        let row = q
            .fetch_one(&self.pool)
            .await
            .map_err(StoreError::Postgres)?;
        use sqlx::Row;
        Ok(Counts {
            passed: row.get::<i64, _>("passed"),
            failed: row.get::<i64, _>("failed"),
            timed_out: row.get::<i64, _>("timed_out"),
            pending: row.get::<i64, _>("pending"),
        })
    }
}
