use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chrono::{DateTime, Duration, Utc};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue, IF_MATCH};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

use super::{
    Counts, Result, StateStore, StoreError, TestCaseRecord, TestStatus, VerificationMode,
    azure_identity::{TokenProvider, WorkloadIdentityConfig, percent_encode_query_value},
};

const COSMOS_SCOPE: &str = "https://cosmos.azure.com/.default";
const COSMOS_VERSION: &str = "2018-12-31";

pub struct CosmosStore {
    http: reqwest::Client,
    endpoint: String,
    database: String,
    container: String,
    auth: CosmosAuth,
}

enum CosmosAuth {
    MasterKey(String),
    WorkloadIdentity(TokenProvider),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct CosmosTestCase {
    id: String,
    #[serde(rename = "_etag", default, skip_serializing_if = "Option::is_none")]
    etag: Option<String>,
    test_id: String,
    blue_green_ref: String,
    triggered_at: DateTime<Utc>,
    trigger_inception_point: String,
    timeout_seconds: i64,
    status: String,
    verdict: Option<bool>,
    verification_mode: String,
    verify_url: String,
    retries_remaining: i32,
    failure_message: Option<String>,
    expires_at: DateTime<Utc>,
}

#[derive(Deserialize)]
struct QueryResponse<T> {
    #[serde(rename = "Documents")]
    documents: Vec<T>,
}

#[derive(Deserialize)]
struct CountRow {
    passed: Option<i64>,
    failed: Option<i64>,
    timed_out: Option<i64>,
    pending: Option<i64>,
}

#[derive(Deserialize)]
struct FailureRow {
    failure_message: Option<String>,
}

impl CosmosStore {
    pub fn new_master_key(endpoint: &str, database: &str, container: &str, key: &str) -> Self {
        Self {
            http: reqwest::Client::new(),
            endpoint: endpoint.trim_end_matches('/').to_string(),
            database: database.to_string(),
            container: container.to_string(),
            auth: CosmosAuth::MasterKey(key.to_string()),
        }
    }

    pub fn new_workload_identity(
        endpoint: &str,
        database: &str,
        container: &str,
        identity: WorkloadIdentityConfig,
    ) -> Self {
        Self {
            http: reqwest::Client::new(),
            endpoint: endpoint.trim_end_matches('/').to_string(),
            database: database.to_string(),
            container: container.to_string(),
            auth: CosmosAuth::WorkloadIdentity(TokenProvider::new(identity)),
        }
    }

    pub fn from_connection_string(value: &str, database: &str, container: &str) -> Result<Self> {
        let parsed = parse_connection_string(value)?;
        Ok(Self::new_master_key(
            &parsed.endpoint,
            database,
            container,
            &parsed.account_key,
        ))
    }

    pub async fn validate_container(&self) -> Result<()> {
        let link = self.container_link();
        let url = format!("{}/{}", self.endpoint, link);
        let headers = self.headers("GET", "colls", &link, None).await?;
        let response = self
            .http
            .get(url)
            .headers(headers)
            .send()
            .await
            .map_err(http_err)?;
        match response.status().as_u16() {
            200 => Ok(()),
            status => Err(StoreError::Other(format!(
                "cosmos container validation failed: {} {}",
                status,
                response.text().await.unwrap_or_default()
            ))),
        }
    }

    fn container_link(&self) -> String {
        format!("dbs/{}/colls/{}", self.database, self.container)
    }

    fn docs_url(&self) -> String {
        format!("{}/{}/docs", self.endpoint, self.container_link())
    }

    fn doc_url(&self, id: &str) -> String {
        format!("{}/{}/docs/{}", self.endpoint, self.container_link(), id)
    }

    async fn headers(
        &self,
        verb: &str,
        resource_type: &str,
        resource_link: &str,
        partition_key: Option<&str>,
    ) -> Result<HeaderMap> {
        let date = Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        let auth = self
            .authorization(verb, resource_type, resource_link, &date)
            .await?;
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, header_value("authorization", &auth)?);
        headers.insert(
            HeaderName::from_static("x-ms-date"),
            header_value("x-ms-date", &date)?,
        );
        headers.insert(
            HeaderName::from_static("x-ms-version"),
            HeaderValue::from_static(COSMOS_VERSION),
        );
        if let Some(partition_key) = partition_key {
            headers.insert(
                HeaderName::from_static("x-ms-documentdb-partitionkey"),
                header_value(
                    "x-ms-documentdb-partitionkey",
                    &serde_json::to_string(&[partition_key]).unwrap(),
                )?,
            );
        }
        Ok(headers)
    }

    async fn authorization(
        &self,
        verb: &str,
        resource_type: &str,
        resource_link: &str,
        date: &str,
    ) -> Result<String> {
        match &self.auth {
            CosmosAuth::MasterKey(key) => {
                cosmos_master_auth(verb, resource_type, resource_link, date, key)
            }
            CosmosAuth::WorkloadIdentity(provider) => {
                let token = provider.access_token(COSMOS_SCOPE).await?;
                Ok(percent_encode_query_value(&format!(
                    "type=aad&ver=1.0&sig={token}"
                )))
            }
        }
    }

    async fn query<T: for<'de> Deserialize<'de>>(
        &self,
        query: &str,
        parameters: Value,
    ) -> Result<Vec<T>> {
        let link = self.container_link();
        let mut headers = self.headers("POST", "docs", &link, None).await?;
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/query+json"),
        );
        headers.insert(
            HeaderName::from_static("x-ms-documentdb-isquery"),
            HeaderValue::from_static("true"),
        );
        headers.insert(
            HeaderName::from_static("x-ms-documentdb-query-enablecrosspartition"),
            HeaderValue::from_static("true"),
        );
        let response = self
            .http
            .post(self.docs_url())
            .headers(headers)
            .json(&json!({ "query": query, "parameters": parameters }))
            .send()
            .await
            .map_err(http_err)?;
        let status = response.status();
        if !status.is_success() {
            return Err(StoreError::Other(format!(
                "cosmos query failed: {} {}",
                status.as_u16(),
                response.text().await.unwrap_or_default()
            )));
        }
        let body = response
            .json::<QueryResponse<T>>()
            .await
            .map_err(|err| StoreError::Other(format!("failed to decode cosmos query: {err}")))?;
        Ok(body.documents)
    }
}

#[async_trait]
impl StateStore for CosmosStore {
    async fn register(&self, run: TestCaseRecord) -> Result<()> {
        let doc = CosmosTestCase::from(run);
        let link = self.container_link();
        let mut headers = self
            .headers("POST", "docs", &link, Some(&doc.blue_green_ref))
            .await?;
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let response = self
            .http
            .post(self.docs_url())
            .headers(headers)
            .json(&doc)
            .send()
            .await
            .map_err(http_err)?;
        match response.status().as_u16() {
            200 | 201 => Ok(()),
            409 => Ok(()),
            status => Err(StoreError::Other(format!(
                "cosmos register failed: {} {}",
                status,
                response.text().await.unwrap_or_default()
            ))),
        }
    }

    async fn get(&self, test_id: &str) -> Result<Option<TestCaseRecord>> {
        let rows = self
            .query::<CosmosTestCase>(
                "SELECT * FROM c WHERE c.test_id = @test_id",
                json!([{ "name": "@test_id", "value": test_id }]),
            )
            .await?;
        Ok(rows.into_iter().next().map(TestCaseRecord::from))
    }

    async fn set_verdict(
        &self,
        test_id: &str,
        passed: bool,
        failure_message: Option<String>,
    ) -> Result<()> {
        let Some(mut doc) = self.get_doc(test_id).await? else {
            return Err(StoreError::NotFound(test_id.to_string()));
        };
        if !is_pending_status(&doc.status) {
            return Err(StoreError::NotFound(test_id.to_string()));
        }
        doc.status = if passed { "Passed" } else { "Failed" }.to_string();
        doc.verdict = Some(passed);
        doc.failure_message = failure_message;
        self.replace_doc(&doc).await
    }

    async fn mark_timed_out(&self, test_id: &str) -> Result<()> {
        let Some(mut doc) = self.get_doc(test_id).await? else {
            return Err(StoreError::NotFound(test_id.to_string()));
        };
        if !is_pending_status(&doc.status) {
            return Err(StoreError::NotFound(test_id.to_string()));
        }
        doc.status = "TimedOut".to_string();
        doc.verdict = None;
        self.replace_doc(&doc).await
    }

    async fn decrement_retries(&self, test_id: &str) -> Result<Option<i32>> {
        let Some(mut doc) = self.get_doc(test_id).await? else {
            return Err(StoreError::NotFound(test_id.to_string()));
        };
        if doc.retries_remaining <= 0 {
            return Ok(None);
        }
        doc.retries_remaining -= 1;
        let remaining = doc.retries_remaining;
        self.replace_doc(&doc).await?;
        Ok(Some(remaining))
    }

    async fn list_pending(&self) -> Result<Vec<TestCaseRecord>> {
        let rows = self
            .query::<CosmosTestCase>(
                "SELECT * FROM c WHERE c.status IN ('Triggered', 'Observing')",
                json!([]),
            )
            .await?;
        Ok(rows.into_iter().map(TestCaseRecord::from).collect())
    }

    async fn list_blue_green_refs(&self) -> Result<BTreeSet<String>> {
        let refs = self
            .query::<String>("SELECT DISTINCT VALUE c.blue_green_ref FROM c", json!([]))
            .await?;
        Ok(refs.into_iter().collect())
    }

    async fn counts(&self, bg: &str) -> Result<Counts> {
        self.counts_for_mode_query(bg, None).await
    }

    async fn counts_for_mode(&self, bg: &str, mode: VerificationMode) -> Result<Counts> {
        self.counts_for_mode_query(bg, Some(mode)).await
    }

    async fn latest_failure_message(&self, bg: &str) -> Result<Option<String>> {
        let rows = self
            .query::<FailureRow>(
                "SELECT TOP 1 c.failure_message FROM c WHERE c.blue_green_ref = @bg AND IS_DEFINED(c.failure_message) AND NOT IS_NULL(c.failure_message) ORDER BY c.triggered_at DESC",
                json!([{ "name": "@bg", "value": bg }]),
            )
            .await?;
        Ok(rows.into_iter().next().and_then(|row| row.failure_message))
    }

    async fn cleanup_blue_green(&self, bg: &str) -> Result<usize> {
        let rows = self
            .query::<CosmosTestCase>(
                "SELECT * FROM c WHERE c.blue_green_ref = @bg",
                json!([{ "name": "@bg", "value": bg }]),
            )
            .await?;
        self.delete_docs(rows).await
    }

    async fn cleanup_expired(&self) -> Result<usize> {
        let rows = self
            .query::<CosmosTestCase>(
                "SELECT * FROM c WHERE c.expires_at < @now AND c.status IN ('Triggered', 'Observing')",
                json!([{ "name": "@now", "value": Utc::now() }]),
            )
            .await?;
        self.delete_docs(rows).await
    }
}

impl CosmosStore {
    async fn delete_docs(&self, rows: Vec<CosmosTestCase>) -> Result<usize> {
        let mut deleted = 0;
        for doc in rows {
            let link = format!("{}/docs/{}", self.container_link(), doc.id);
            let headers = self
                .headers("DELETE", "docs", &link, Some(&doc.blue_green_ref))
                .await?;
            let response = self
                .http
                .delete(self.doc_url(&doc.id))
                .headers(headers)
                .send()
                .await
                .map_err(http_err)?;
            if response.status().is_success() {
                deleted += 1;
            }
        }
        Ok(deleted)
    }

    async fn get_doc(&self, test_id: &str) -> Result<Option<CosmosTestCase>> {
        let rows = self
            .query::<CosmosTestCase>(
                "SELECT * FROM c WHERE c.test_id = @test_id",
                json!([{ "name": "@test_id", "value": test_id }]),
            )
            .await?;
        Ok(rows.into_iter().next())
    }

    async fn replace_doc(&self, doc: &CosmosTestCase) -> Result<()> {
        let link = format!("{}/docs/{}", self.container_link(), doc.id);
        let mut headers = self
            .headers("PUT", "docs", &link, Some(&doc.blue_green_ref))
            .await?;
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if let Some(etag) = doc.etag.as_deref() {
            headers.insert(IF_MATCH, header_value("if-match", etag)?);
        }
        let response = self
            .http
            .put(self.doc_url(&doc.id))
            .headers(headers)
            .json(doc)
            .send()
            .await
            .map_err(http_err)?;
        if response.status().is_success() {
            return Ok(());
        }
        if response.status().as_u16() == 412 {
            return Err(StoreError::Conflict(doc.test_id.clone()));
        }
        Err(StoreError::Other(format!(
            "cosmos replace failed: {} {}",
            response.status().as_u16(),
            response.text().await.unwrap_or_default()
        )))
    }

    async fn counts_for_mode_query(
        &self,
        bg: &str,
        mode: Option<VerificationMode>,
    ) -> Result<Counts> {
        let mut predicate = "c.blue_green_ref = @bg".to_string();
        let mut params = vec![json!({ "name": "@bg", "value": bg })];
        if let Some(mode) = mode {
            predicate.push_str(" AND c.verification_mode = @mode");
            params.push(json!({
                "name": "@mode",
                "value": match mode {
                    VerificationMode::Data => "Data",
                    VerificationMode::Custom => "Custom",
                }
            }));
        }
        let query = format!(
            "SELECT VALUE {{
                passed: SUM(IIF(c.status = 'Passed', 1, 0)),
                failed: SUM(IIF(c.status = 'Failed', 1, 0)),
                timed_out: SUM(IIF(c.status = 'TimedOut', 1, 0)),
                pending: SUM(IIF(c.status IN ('Triggered', 'Observing'), 1, 0))
            }} FROM c WHERE {predicate}"
        );
        self.counts_query(&query, Value::Array(params)).await
    }

    async fn counts_query(&self, query: &str, parameters: Value) -> Result<Counts> {
        let rows = self.query::<CountRow>(query, parameters).await?;
        let row = rows.into_iter().next().unwrap_or(CountRow {
            passed: Some(0),
            failed: Some(0),
            timed_out: Some(0),
            pending: Some(0),
        });
        Ok(Counts {
            passed: row.passed.unwrap_or(0),
            failed: row.failed.unwrap_or(0),
            timed_out: row.timed_out.unwrap_or(0),
            pending: row.pending.unwrap_or(0),
        })
    }
}

impl From<TestCaseRecord> for CosmosTestCase {
    fn from(run: TestCaseRecord) -> Self {
        let expires_at = run.expires_at();
        let status = match run.status {
            TestStatus::Triggered => "Triggered",
            TestStatus::Observing => "Observing",
            TestStatus::Passed => "Passed",
            TestStatus::Failed => "Failed",
            TestStatus::TimedOut => "TimedOut",
        }
        .to_string();
        let verification_mode = match run.verification_mode {
            VerificationMode::Data => "Data",
            VerificationMode::Custom => "Custom",
        }
        .to_string();
        Self {
            id: run.test_id.clone(),
            etag: None,
            test_id: run.test_id,
            blue_green_ref: run.blue_green_ref,
            triggered_at: run.triggered_at,
            trigger_inception_point: run.source_inception_point,
            timeout_seconds: run.timeout.num_seconds(),
            status,
            verdict: run.verdict,
            verification_mode,
            verify_url: run.verify_url,
            retries_remaining: run.retries_remaining,
            failure_message: run.failure_message,
            expires_at,
        }
    }
}

impl From<CosmosTestCase> for TestCaseRecord {
    fn from(doc: CosmosTestCase) -> Self {
        Self {
            test_id: doc.test_id,
            blue_green_ref: doc.blue_green_ref,
            triggered_at: doc.triggered_at,
            source_inception_point: doc.trigger_inception_point,
            timeout: Duration::seconds(doc.timeout_seconds),
            status: match doc.status.as_str() {
                "Observing" => TestStatus::Observing,
                "Passed" => TestStatus::Passed,
                "Failed" => TestStatus::Failed,
                "TimedOut" => TestStatus::TimedOut,
                _ => TestStatus::Triggered,
            },
            verdict: doc.verdict,
            verification_mode: match doc.verification_mode.as_str() {
                "Custom" => VerificationMode::Custom,
                _ => VerificationMode::Data,
            },
            verify_url: doc.verify_url,
            retries_remaining: doc.retries_remaining,
            failure_message: doc.failure_message,
        }
    }
}

struct ParsedConnectionString {
    endpoint: String,
    account_key: String,
}

fn parse_connection_string(value: &str) -> Result<ParsedConnectionString> {
    let mut endpoint = None;
    let mut account_key = None;
    for part in value.split(';').filter(|part| !part.is_empty()) {
        let Some((name, value)) = part.split_once('=') else {
            continue;
        };
        match name {
            "AccountEndpoint" => endpoint = Some(value.to_string()),
            "AccountKey" => account_key = Some(value.to_string()),
            _ => {}
        }
    }
    Ok(ParsedConnectionString {
        endpoint: endpoint.ok_or_else(|| {
            StoreError::Other("cosmos connection string missing AccountEndpoint".to_string())
        })?,
        account_key: account_key.ok_or_else(|| {
            StoreError::Other("cosmos connection string missing AccountKey".to_string())
        })?,
    })
}

fn cosmos_master_auth(
    verb: &str,
    resource_type: &str,
    resource_link: &str,
    date: &str,
    key: &str,
) -> Result<String> {
    let payload = format!(
        "{}\n{}\n{}\n{}\n\n",
        verb.to_ascii_lowercase(),
        resource_type.to_ascii_lowercase(),
        resource_link,
        date.to_ascii_lowercase()
    );
    let key = STANDARD
        .decode(key)
        .map_err(|err| StoreError::Other(format!("invalid cosmos account key: {err}")))?;
    let signature = STANDARD.encode(hmac_sha256(&key, payload.as_bytes()));
    Ok(percent_encode_query_value(&format!(
        "type=master&ver=1.0&sig={signature}"
    )))
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK_SIZE: usize = 64;
    let mut key_block = [0u8; BLOCK_SIZE];
    if key.len() > BLOCK_SIZE {
        key_block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }

    let mut ipad = [0x36u8; BLOCK_SIZE];
    let mut opad = [0x5cu8; BLOCK_SIZE];
    for i in 0..BLOCK_SIZE {
        ipad[i] ^= key_block[i];
        opad[i] ^= key_block[i];
    }

    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(message);
    let inner_hash = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_hash);
    outer.finalize().into()
}

fn header_value(name: &str, value: &str) -> Result<HeaderValue> {
    HeaderValue::from_str(value)
        .map_err(|err| StoreError::Other(format!("invalid {name} header value: {err}")))
}

fn http_err(err: reqwest::Error) -> StoreError {
    StoreError::Other(format!("cosmos http request failed: {err}"))
}

fn is_pending_status(status: &str) -> bool {
    matches!(status, "Triggered" | "Observing")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cosmos_connection_string() {
        let parsed = parse_connection_string(
            "AccountEndpoint=https://acct.documents.azure.com:443/;AccountKey=abc==;",
        )
        .unwrap();
        assert_eq!(parsed.endpoint, "https://acct.documents.azure.com:443/");
        assert_eq!(parsed.account_key, "abc==");
    }

    #[test]
    fn aad_authorization_header_is_encoded() {
        let encoded = percent_encode_query_value("type=aad&ver=1.0&sig=abc+/=");
        assert!(encoded.contains("type%3Daad"));
        assert!(encoded.contains("sig%3Dabc%2B%2F%3D"));
    }
}
