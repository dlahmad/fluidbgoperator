use chrono::{DateTime, Duration, Utc};
use reqwest::header::CONTENT_TYPE;
use serde::Deserialize;
use tokio::sync::Mutex;

use super::{Result, StoreError};

#[derive(Clone)]
pub struct WorkloadIdentityConfig {
    pub tenant_id: String,
    pub client_id: String,
    pub token_file: String,
    pub authority_host: String,
}

pub struct TokenProvider {
    http: reqwest::Client,
    config: WorkloadIdentityConfig,
    token: Mutex<Option<CachedToken>>,
}

#[derive(Clone)]
struct CachedToken {
    access_token: String,
    expires_at: DateTime<Utc>,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: Option<i64>,
}

impl TokenProvider {
    pub fn new(config: WorkloadIdentityConfig) -> Self {
        Self {
            http: reqwest::Client::new(),
            config,
            token: Mutex::new(None),
        }
    }

    pub async fn access_token(&self, scope: &str) -> Result<String> {
        let mut token = self.token.lock().await;
        if let Some(cached) = token.as_ref()
            && cached.expires_at > Utc::now() + Duration::minutes(5)
        {
            return Ok(cached.access_token.clone());
        }
        let fresh = request_workload_identity_token(&self.http, &self.config, scope).await?;
        let access_token = fresh.access_token.clone();
        *token = Some(fresh);
        Ok(access_token)
    }
}

pub fn workload_identity_from_env(prefix: &str) -> WorkloadIdentityConfig {
    WorkloadIdentityConfig {
        tenant_id: required_env(prefix, "TENANT_ID", "AZURE_TENANT_ID"),
        client_id: required_env(prefix, "CLIENT_ID", "AZURE_CLIENT_ID"),
        token_file: required_env(prefix, "FEDERATED_TOKEN_FILE", "AZURE_FEDERATED_TOKEN_FILE"),
        authority_host: std::env::var(format!("{prefix}_AUTHORITY_HOST"))
            .or_else(|_| std::env::var("AZURE_AUTHORITY_HOST"))
            .unwrap_or_else(|_| "https://login.microsoftonline.com".to_string()),
    }
}

fn required_env(prefix: &str, name: &str, fallback: &str) -> String {
    std::env::var(format!("{prefix}_{name}"))
        .or_else(|_| std::env::var(fallback))
        .unwrap_or_else(|_| panic!("{prefix}_{name} or {fallback} is required"))
}

async fn request_workload_identity_token(
    http: &reqwest::Client,
    config: &WorkloadIdentityConfig,
    scope: &str,
) -> Result<CachedToken> {
    let assertion = std::fs::read_to_string(&config.token_file).map_err(|err| {
        StoreError::Other(format!(
            "failed to read federated token file '{}': {err}",
            config.token_file
        ))
    })?;
    let url = format!(
        "{}/{}/oauth2/v2.0/token",
        config.authority_host.trim_end_matches('/'),
        percent_encode_path_segment(&config.tenant_id)
    );
    let body = form_urlencoded(&[
        ("client_id", config.client_id.as_str()),
        ("scope", scope),
        ("grant_type", "client_credentials"),
        (
            "client_assertion_type",
            "urn:ietf:params:oauth:client-assertion-type:jwt-bearer",
        ),
        ("client_assertion", assertion.trim()),
    ]);
    let response = http
        .post(url)
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .map_err(|err| {
            StoreError::Other(format!("workload identity token request failed: {err}"))
        })?;
    let status = response.status();
    if !status.is_success() {
        return Err(StoreError::Other(format!(
            "workload identity token request failed: {} {}",
            status.as_u16(),
            response.text().await.unwrap_or_default()
        )));
    }
    let token = response.json::<TokenResponse>().await.map_err(|err| {
        StoreError::Other(format!("failed to decode workload identity token: {err}"))
    })?;
    Ok(CachedToken {
        access_token: token.access_token,
        expires_at: Utc::now() + Duration::seconds(token.expires_in.unwrap_or(3600)),
    })
}

pub fn form_urlencoded(fields: &[(&str, &str)]) -> String {
    fields
        .iter()
        .map(|(key, value)| {
            format!(
                "{}={}",
                percent_encode_query_value(key),
                percent_encode_query_value(value)
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

pub fn percent_encode_path_segment(value: &str) -> String {
    percent_encode(value.as_bytes(), is_unreserved)
}

pub fn percent_encode_query_value(value: &str) -> String {
    percent_encode(value.as_bytes(), is_unreserved)
}

fn percent_encode(value: &[u8], keep: fn(u8) -> bool) -> String {
    let mut out = String::new();
    for &byte in value {
        if keep(byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn is_unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
}
