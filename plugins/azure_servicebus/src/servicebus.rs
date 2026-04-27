use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chrono::{DateTime, Duration, Utc};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue, LOCATION};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::config::{AuthMode, Config, ManagementConfig, QueueDeclarationConfig};

const SERVICE_BUS_SCOPE: &str = "https://servicebus.azure.net/.default";
const ARM_SCOPE: &str = "https://management.azure.com/.default";

#[derive(Clone)]
pub(crate) struct ServiceBusClient {
    http: reqwest::Client,
    endpoint: String,
    namespace_name: String,
    auth: AuthProvider,
    management: Option<ManagementConfig>,
}

#[derive(Clone)]
enum AuthProvider {
    Sas {
        key_name: String,
        key: String,
        ttl_seconds: i64,
    },
    WorkloadIdentity {
        tenant_id: String,
        client_id: String,
        token_file: String,
        authority_host: String,
        service_bus_token: Arc<Mutex<Option<CachedToken>>>,
        arm_token: Arc<Mutex<Option<CachedToken>>>,
    },
}

#[derive(Clone)]
struct CachedToken {
    access_token: String,
    expires_at: DateTime<Utc>,
}

pub(crate) struct LockedMessage {
    pub(crate) body: Vec<u8>,
    pub(crate) properties: BTreeMap<String, String>,
    location: String,
}

impl ServiceBusClient {
    pub(crate) fn from_config(config: &Config) -> Result<Self> {
        let connection = config
            .connection_string
            .as_deref()
            .map(parse_connection_string);
        let connection = match connection {
            Some(result) => Some(result?),
            None => None,
        };
        let auth_mode = config
            .auth
            .as_ref()
            .and_then(|auth| auth.mode)
            .unwrap_or_else(|| {
                if connection.is_some() {
                    AuthMode::ConnectionString
                } else {
                    AuthMode::WorkloadIdentity
                }
            });
        let namespace = config
            .fully_qualified_namespace
            .clone()
            .or_else(|| {
                connection
                    .as_ref()
                    .map(|conn| conn.fully_qualified_namespace.clone())
            })
            .context("missing fullyQualifiedNamespace or connectionString Endpoint")?;
        let endpoint = format!(
            "https://{}",
            namespace
                .trim_start_matches("sb://")
                .trim_start_matches("https://")
                .trim_end_matches('/')
        );
        let namespace_name = namespace
            .trim_start_matches("sb://")
            .trim_start_matches("https://")
            .trim_end_matches('/')
            .split('.')
            .next()
            .unwrap_or_default()
            .to_string();

        let auth = match auth_mode {
            AuthMode::ConnectionString => {
                let connection =
                    connection.context("connectionString auth requires connectionString")?;
                AuthProvider::Sas {
                    key_name: connection.shared_access_key_name,
                    key: connection.shared_access_key,
                    ttl_seconds: config
                        .auth
                        .as_ref()
                        .and_then(|auth| auth.sas_token_ttl_seconds)
                        .unwrap_or(3600),
                }
            }
            AuthMode::WorkloadIdentity => {
                let auth = config.auth.as_ref();
                AuthProvider::WorkloadIdentity {
                    tenant_id: auth
                        .and_then(|auth| auth.tenant_id.clone())
                        .or_else(|| std::env::var("AZURE_TENANT_ID").ok())
                        .context("workloadIdentity auth requires tenantId or AZURE_TENANT_ID")?,
                    client_id: auth
                        .and_then(|auth| auth.client_id.clone())
                        .or_else(|| std::env::var("AZURE_CLIENT_ID").ok())
                        .context("workloadIdentity auth requires clientId or AZURE_CLIENT_ID")?,
                    token_file: auth
                        .and_then(|auth| auth.federated_token_file.clone())
                        .or_else(|| std::env::var("AZURE_FEDERATED_TOKEN_FILE").ok())
                        .context(
                            "workloadIdentity auth requires federatedTokenFile or AZURE_FEDERATED_TOKEN_FILE",
                        )?,
                    authority_host: auth
                        .and_then(|auth| auth.authority_host.clone())
                        .or_else(|| std::env::var("AZURE_AUTHORITY_HOST").ok())
                        .unwrap_or_else(|| "https://login.microsoftonline.com".to_string()),
                    service_bus_token: Arc::new(Mutex::new(None)),
                    arm_token: Arc::new(Mutex::new(None)),
                }
            }
        };

        Ok(Self {
            http: reqwest::Client::new(),
            endpoint,
            namespace_name,
            auth,
            management: config.management.clone(),
        })
    }

    pub(crate) async fn create_queue(
        &self,
        queue: &str,
        declaration: &QueueDeclarationConfig,
    ) -> Result<()> {
        if self.uses_workload_identity()
            && let Some(management) = &self.management
            && has_arm_management_config(management)
        {
            if !management.create_queues {
                return Ok(());
            }
            return self.create_queue_arm(queue, management, declaration).await;
        }

        let url = self.queue_url(queue);
        let body = queue_description_xml(declaration);
        let response = self
            .authorized(
                self.http
                    .put(&url)
                    .header(
                        CONTENT_TYPE,
                        "application/atom+xml;type=entry;charset=utf-8",
                    )
                    .body(body),
                &url,
                SERVICE_BUS_SCOPE,
            )
            .await?
            .send()
            .await?;
        match response.status().as_u16() {
            200 | 201 | 409 => Ok(()),
            status => bail!(
                "create Service Bus queue '{}' failed: {} {}",
                queue,
                status,
                response.text().await.unwrap_or_default()
            ),
        }
    }

    pub(crate) async fn delete_queue(&self, queue: &str) -> Result<()> {
        if self.uses_workload_identity()
            && let Some(management) = &self.management
            && has_arm_management_config(management)
        {
            if !management.delete_queues {
                return Ok(());
            }
            return self.delete_queue_arm(queue, management).await;
        }

        let url = self.queue_url(queue);
        let response = self
            .authorized(self.http.delete(&url), &url, SERVICE_BUS_SCOPE)
            .await?
            .send()
            .await?;
        match response.status().as_u16() {
            200 | 202 | 204 | 404 => Ok(()),
            status => bail!(
                "delete Service Bus queue '{}' failed: {} {}",
                queue,
                status,
                response.text().await.unwrap_or_default()
            ),
        }
    }

    pub(crate) async fn send_message(
        &self,
        queue: &str,
        payload: &[u8],
        properties: &BTreeMap<String, String>,
    ) -> Result<()> {
        let queue_url = self.queue_url(queue);
        let url = format!("{queue_url}/messages");
        let mut builder = self
            .http
            .post(&url)
            .header(CONTENT_TYPE, "application/octet-stream")
            .body(payload.to_vec());
        for (key, value) in properties {
            builder = builder.header(key, value);
        }
        let response = self
            .authorized(builder, &queue_url, SERVICE_BUS_SCOPE)
            .await?
            .send()
            .await?;
        match response.status().as_u16() {
            200 | 201 => Ok(()),
            status => bail!(
                "send Service Bus message to '{}' failed: {} {}",
                queue,
                status,
                response.text().await.unwrap_or_default()
            ),
        }
    }

    pub(crate) async fn receive_peek_lock(
        &self,
        queue: &str,
        timeout_seconds: u32,
    ) -> Result<Option<LockedMessage>> {
        let queue_url = self.queue_url(queue);
        let url = format!("{queue_url}/messages/head?timeout={timeout_seconds}");
        let response = self
            .authorized(self.http.post(&url), &queue_url, SERVICE_BUS_SCOPE)
            .await?
            .send()
            .await?;
        match response.status().as_u16() {
            201 => {
                let location = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_string)
                    .context("Service Bus locked message response missing Location")?;
                let properties = custom_properties(response.headers());
                let body = response.bytes().await?.to_vec();
                Ok(Some(LockedMessage {
                    body,
                    properties,
                    location,
                }))
            }
            204 => Ok(None),
            status => bail!(
                "receive Service Bus message from '{}' failed: {} {}",
                queue,
                status,
                response.text().await.unwrap_or_default()
            ),
        }
    }

    pub(crate) async fn complete(&self, message: &LockedMessage) -> Result<()> {
        let resource = strip_query(&message.location);
        let response = self
            .authorized(
                self.http.delete(&message.location),
                resource,
                SERVICE_BUS_SCOPE,
            )
            .await?
            .send()
            .await?;
        match response.status().as_u16() {
            200 | 204 => Ok(()),
            status => bail!(
                "complete Service Bus message failed: {} {}",
                status,
                response.text().await.unwrap_or_default()
            ),
        }
    }

    pub(crate) async fn abandon(&self, message: &LockedMessage) -> Result<()> {
        let resource = strip_query(&message.location);
        let response = self
            .authorized(
                self.http.put(&message.location),
                resource,
                SERVICE_BUS_SCOPE,
            )
            .await?
            .send()
            .await?;
        match response.status().as_u16() {
            200 | 204 => Ok(()),
            status => bail!(
                "unlock Service Bus message failed: {} {}",
                status,
                response.text().await.unwrap_or_default()
            ),
        }
    }

    pub(crate) async fn move_available_messages(
        &self,
        source_queue: &str,
        target_queue: &str,
    ) -> Result<u32> {
        self.move_available_messages_from_path(source_queue, target_queue)
            .await
    }

    pub(crate) async fn move_available_dead_letter_messages(
        &self,
        source_queue: &str,
        target_queue: &str,
    ) -> Result<u32> {
        self.move_available_messages_from_path(&dead_letter_path(source_queue), target_queue)
            .await
    }

    async fn move_available_messages_from_path(
        &self,
        source_path: &str,
        target_queue: &str,
    ) -> Result<u32> {
        let mut moved = 0;
        loop {
            let Some(message) = self.receive_peek_lock(source_path, 1).await? else {
                break;
            };
            match self
                .send_message(target_queue, &message.body, &message.properties)
                .await
            {
                Ok(()) => {
                    self.complete(&message).await?;
                    moved += 1;
                }
                Err(err) => {
                    let _ = self.abandon(&message).await;
                    return Err(err);
                }
            }
        }
        Ok(moved)
    }

    pub(crate) async fn queue_drain_message_count(&self, queue: &str) -> Result<u64> {
        if self.uses_workload_identity()
            && let Some(management) = &self.management
            && has_arm_management_config(management)
        {
            return self.queue_drain_message_count_arm(queue, management).await;
        }

        let active = self.queue_message_count(queue).await?;
        let dead_letter = self.queue_message_count(&dead_letter_path(queue)).await?;
        Ok(active + dead_letter)
    }

    pub(crate) async fn queue_message_count(&self, queue: &str) -> Result<u64> {
        if self.uses_workload_identity()
            && let Some(management) = &self.management
            && has_arm_management_config(management)
        {
            return self.queue_message_count_arm(queue, management).await;
        }

        let url = self.queue_url(queue);
        let response = self
            .authorized(self.http.get(&url), &url, SERVICE_BUS_SCOPE)
            .await?
            .send()
            .await?;
        match response.status().as_u16() {
            200 => {
                let body = response.text().await?;
                Ok(extract_xml_u64(&body, "MessageCount")
                    .or_else(|| extract_xml_u64(&body, "QueueDepth"))
                    .unwrap_or(0))
            }
            404 => Ok(0),
            status => bail!(
                "get Service Bus queue '{}' failed: {} {}",
                queue,
                status,
                response.text().await.unwrap_or_default()
            ),
        }
    }

    async fn create_queue_arm(
        &self,
        queue: &str,
        management: &ManagementConfig,
        declaration: &QueueDeclarationConfig,
    ) -> Result<()> {
        let url = self.arm_queue_url(queue, management)?;
        let body =
            serde_json::json!({ "properties": queue_description_arm_properties(declaration) });
        let response = self
            .authorized(self.http.put(&url).json(&body), &url, ARM_SCOPE)
            .await?
            .send()
            .await?;
        match response.status().as_u16() {
            200 | 201 | 202 | 409 => Ok(()),
            status => bail!(
                "ARM create Service Bus queue '{}' failed: {} {}",
                queue,
                status,
                response.text().await.unwrap_or_default()
            ),
        }
    }

    async fn delete_queue_arm(&self, queue: &str, management: &ManagementConfig) -> Result<()> {
        let url = self.arm_queue_url(queue, management)?;
        let response = self
            .authorized(self.http.delete(&url), &url, ARM_SCOPE)
            .await?
            .send()
            .await?;
        match response.status().as_u16() {
            200 | 202 | 204 | 404 => Ok(()),
            status => bail!(
                "ARM delete Service Bus queue '{}' failed: {} {}",
                queue,
                status,
                response.text().await.unwrap_or_default()
            ),
        }
    }

    async fn queue_message_count_arm(
        &self,
        queue: &str,
        management: &ManagementConfig,
    ) -> Result<u64> {
        let url = self.arm_queue_url(queue, management)?;
        let response = self
            .authorized(self.http.get(&url), &url, ARM_SCOPE)
            .await?
            .send()
            .await?;
        match response.status().as_u16() {
            200 => {
                let body = response.json::<Value>().await?;
                Ok(body
                    .pointer("/properties/messageCount")
                    .and_then(Value::as_u64)
                    .unwrap_or(0))
            }
            404 => Ok(0),
            status => bail!(
                "ARM get Service Bus queue '{}' failed: {} {}",
                queue,
                status,
                response.text().await.unwrap_or_default()
            ),
        }
    }

    async fn queue_drain_message_count_arm(
        &self,
        queue: &str,
        management: &ManagementConfig,
    ) -> Result<u64> {
        let url = self.arm_queue_url(queue, management)?;
        let response = self
            .authorized(self.http.get(&url), &url, ARM_SCOPE)
            .await?
            .send()
            .await?;
        match response.status().as_u16() {
            200 => {
                let body = response.json::<Value>().await?;
                let active = body
                    .pointer("/properties/messageCountDetails/activeMessageCount")
                    .and_then(Value::as_u64);
                let dead_letter = body
                    .pointer("/properties/messageCountDetails/deadLetterMessageCount")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let detailed_total = active.map(|active| active + dead_letter).unwrap_or(0);
                let total = body
                    .pointer("/properties/messageCount")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                Ok(total.max(detailed_total))
            }
            404 => Ok(0),
            status => bail!(
                "ARM get Service Bus queue '{}' failed: {} {}",
                queue,
                status,
                response.text().await.unwrap_or_default()
            ),
        }
    }

    fn arm_queue_url(&self, queue: &str, management: &ManagementConfig) -> Result<String> {
        let subscription = required(&management.subscription_id, "management.subscriptionId")?;
        let resource_group = required(&management.resource_group, "management.resourceGroup")?;
        let namespace = management
            .namespace_name
            .as_deref()
            .unwrap_or(&self.namespace_name);
        Ok(format!(
            "https://management.azure.com/subscriptions/{}/resourceGroups/{}/providers/Microsoft.ServiceBus/namespaces/{}/queues/{}?api-version=2024-01-01",
            percent_encode_path_segment(subscription),
            percent_encode_path_segment(resource_group),
            percent_encode_path_segment(namespace),
            percent_encode_path_segment(queue)
        ))
    }

    fn queue_url(&self, queue: &str) -> String {
        format!("{}/{}", self.endpoint, percent_encode_path(queue))
    }

    fn uses_workload_identity(&self) -> bool {
        matches!(self.auth, AuthProvider::WorkloadIdentity { .. })
    }

    async fn authorized(
        &self,
        builder: reqwest::RequestBuilder,
        resource: &str,
        scope: &str,
    ) -> Result<reqwest::RequestBuilder> {
        Ok(builder.header(
            AUTHORIZATION,
            self.authorization_header(resource, scope).await?,
        ))
    }

    async fn authorization_header(&self, resource: &str, scope: &str) -> Result<String> {
        match &self.auth {
            AuthProvider::Sas {
                key_name,
                key,
                ttl_seconds,
            } => Ok(sas_token(resource, key_name, key, *ttl_seconds)),
            AuthProvider::WorkloadIdentity {
                tenant_id,
                client_id,
                token_file,
                authority_host,
                service_bus_token,
                arm_token,
            } => {
                let token_cache = if scope == ARM_SCOPE {
                    arm_token
                } else {
                    service_bus_token
                };
                let mut token = token_cache.lock().await;
                if let Some(cached) = token.as_ref()
                    && cached.expires_at > Utc::now() + Duration::minutes(5)
                {
                    return Ok(format!("Bearer {}", cached.access_token));
                }
                let fresh = request_workload_identity_token(
                    &self.http,
                    authority_host,
                    tenant_id,
                    client_id,
                    token_file,
                    scope,
                )
                .await?;
                let header = format!("Bearer {}", fresh.access_token);
                *token = Some(fresh);
                Ok(header)
            }
        }
    }
}

fn has_arm_management_config(management: &ManagementConfig) -> bool {
    management.subscription_id.is_some() && management.resource_group.is_some()
}

fn required<'a>(value: &'a Option<String>, name: &str) -> Result<&'a str> {
    value.as_deref().with_context(|| format!("missing {name}"))
}

#[derive(Debug)]
struct ParsedConnectionString {
    fully_qualified_namespace: String,
    shared_access_key_name: String,
    shared_access_key: String,
}

fn parse_connection_string(value: &str) -> Result<ParsedConnectionString> {
    let mut endpoint = None;
    let mut key_name = None;
    let mut key = None;
    for part in value.split(';').filter(|part| !part.is_empty()) {
        let Some((name, value)) = part.split_once('=') else {
            continue;
        };
        match name {
            "Endpoint" => endpoint = Some(value.to_string()),
            "SharedAccessKeyName" => key_name = Some(value.to_string()),
            "SharedAccessKey" => key = Some(value.to_string()),
            _ => {}
        }
    }
    let endpoint = endpoint.context("connection string missing Endpoint")?;
    let fully_qualified_namespace = endpoint
        .trim_start_matches("sb://")
        .trim_start_matches("https://")
        .trim_end_matches('/')
        .to_string();
    Ok(ParsedConnectionString {
        fully_qualified_namespace,
        shared_access_key_name: key_name
            .context("connection string missing SharedAccessKeyName")?,
        shared_access_key: key.context("connection string missing SharedAccessKey")?,
    })
}

fn sas_token(resource: &str, key_name: &str, key: &str, ttl_seconds: i64) -> String {
    let encoded_resource = percent_encode_query_value(resource);
    let expiry = (Utc::now() + Duration::seconds(ttl_seconds)).timestamp();
    let string_to_sign = format!("{encoded_resource}\n{expiry}");
    let signature = hmac_sha256(key.as_bytes(), string_to_sign.as_bytes());
    let signature = percent_encode_query_value(&STANDARD.encode(signature));
    format!(
        "SharedAccessSignature sr={encoded_resource}&sig={signature}&se={expiry}&skn={key_name}"
    )
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: Option<i64>,
}

async fn request_workload_identity_token(
    http: &reqwest::Client,
    authority_host: &str,
    tenant_id: &str,
    client_id: &str,
    token_file: &str,
    scope: &str,
) -> Result<CachedToken> {
    let assertion = std::fs::read_to_string(token_file)
        .with_context(|| format!("failed to read federated token file '{token_file}'"))?;
    let url = format!(
        "{}/{}/oauth2/v2.0/token",
        authority_host.trim_end_matches('/'),
        percent_encode_path_segment(tenant_id)
    );
    let body = form_urlencoded(&[
        ("client_id", client_id),
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
        .await?;
    let status = response.status();
    if !status.is_success() {
        bail!(
            "workload identity token request failed: {} {}",
            status.as_u16(),
            response.text().await.unwrap_or_default()
        );
    }
    let token = response.json::<TokenResponse>().await?;
    Ok(CachedToken {
        access_token: token.access_token,
        expires_at: Utc::now() + Duration::seconds(token.expires_in.unwrap_or(3600)),
    })
}

fn queue_description_xml(declaration: &QueueDeclarationConfig) -> String {
    let properties = queue_description_fields(declaration)
        .into_iter()
        .map(|(name, value)| format!("      <{name}>{}</{name}>", xml_escape(&value)))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<entry xmlns="http://www.w3.org/2005/Atom">
  <content type="application/xml">
    <QueueDescription xmlns="http://schemas.microsoft.com/netservices/2010/10/servicebus/connect">
{properties}
    </QueueDescription>
  </content>
</entry>"#
    )
}

fn queue_description_arm_properties(declaration: &QueueDeclarationConfig) -> serde_json::Value {
    let mut properties = serde_json::Map::new();
    properties.insert(
        "lockDuration".to_string(),
        serde_json::Value::String(
            declaration
                .lock_duration
                .clone()
                .unwrap_or_else(|| "PT1M".to_string()),
        ),
    );
    properties.insert(
        "enableBatchedOperations".to_string(),
        serde_json::Value::Bool(declaration.enable_batched_operations.unwrap_or(true)),
    );
    insert_json_property(
        &mut properties,
        "maxSizeInMegabytes",
        declaration.max_size_in_megabytes,
    );
    insert_json_property(
        &mut properties,
        "maxMessageSizeInKilobytes",
        declaration.max_message_size_in_kilobytes,
    );
    insert_json_property(
        &mut properties,
        "requiresDuplicateDetection",
        declaration.requires_duplicate_detection,
    );
    insert_json_property(
        &mut properties,
        "requiresSession",
        declaration.requires_session,
    );
    insert_json_property(
        &mut properties,
        "defaultMessageTimeToLive",
        declaration.default_message_time_to_live.as_deref(),
    );
    insert_json_property(
        &mut properties,
        "deadLetteringOnMessageExpiration",
        declaration.dead_lettering_on_message_expiration,
    );
    insert_json_property(
        &mut properties,
        "duplicateDetectionHistoryTimeWindow",
        declaration
            .duplicate_detection_history_time_window
            .as_deref(),
    );
    insert_json_property(
        &mut properties,
        "maxDeliveryCount",
        declaration.max_delivery_count,
    );
    insert_json_property(
        &mut properties,
        "autoDeleteOnIdle",
        declaration.auto_delete_on_idle.as_deref(),
    );
    insert_json_property(
        &mut properties,
        "enablePartitioning",
        declaration.enable_partitioning,
    );
    insert_json_property(&mut properties, "enableExpress", declaration.enable_express);
    insert_json_property(
        &mut properties,
        "forwardTo",
        declaration.forward_to.as_deref(),
    );
    insert_json_property(
        &mut properties,
        "forwardDeadLetteredMessagesTo",
        declaration.forward_dead_lettered_messages_to.as_deref(),
    );
    insert_json_property(&mut properties, "status", declaration.status.as_deref());
    serde_json::Value::Object(properties)
}

fn queue_description_fields(declaration: &QueueDeclarationConfig) -> Vec<(&'static str, String)> {
    let mut fields = vec![
        (
            "LockDuration",
            declaration
                .lock_duration
                .clone()
                .unwrap_or_else(|| "PT1M".to_string()),
        ),
        (
            "EnableBatchedOperations",
            declaration
                .enable_batched_operations
                .unwrap_or(true)
                .to_string(),
        ),
    ];
    push_xml_field(
        &mut fields,
        "MaxSizeInMegabytes",
        declaration.max_size_in_megabytes,
    );
    push_xml_field(
        &mut fields,
        "MaxMessageSizeInKilobytes",
        declaration.max_message_size_in_kilobytes,
    );
    push_xml_field(
        &mut fields,
        "RequiresDuplicateDetection",
        declaration.requires_duplicate_detection,
    );
    push_xml_field(&mut fields, "RequiresSession", declaration.requires_session);
    push_xml_field(
        &mut fields,
        "DefaultMessageTimeToLive",
        declaration.default_message_time_to_live.as_deref(),
    );
    push_xml_field(
        &mut fields,
        "DeadLetteringOnMessageExpiration",
        declaration.dead_lettering_on_message_expiration,
    );
    push_xml_field(
        &mut fields,
        "DuplicateDetectionHistoryTimeWindow",
        declaration
            .duplicate_detection_history_time_window
            .as_deref(),
    );
    push_xml_field(
        &mut fields,
        "MaxDeliveryCount",
        declaration.max_delivery_count,
    );
    push_xml_field(
        &mut fields,
        "AutoDeleteOnIdle",
        declaration.auto_delete_on_idle.as_deref(),
    );
    push_xml_field(
        &mut fields,
        "EnablePartitioning",
        declaration.enable_partitioning,
    );
    push_xml_field(&mut fields, "EnableExpress", declaration.enable_express);
    push_xml_field(&mut fields, "ForwardTo", declaration.forward_to.as_deref());
    push_xml_field(
        &mut fields,
        "ForwardDeadLetteredMessagesTo",
        declaration.forward_dead_lettered_messages_to.as_deref(),
    );
    push_xml_field(&mut fields, "Status", declaration.status.as_deref());
    fields
}

fn insert_json_property<T: serde::Serialize>(
    target: &mut serde_json::Map<String, serde_json::Value>,
    name: &str,
    value: Option<T>,
) {
    if let Some(value) = value
        && let Ok(value) = serde_json::to_value(value)
    {
        target.insert(name.to_string(), value);
    }
}

fn push_xml_field<T: ToString>(
    target: &mut Vec<(&'static str, String)>,
    name: &'static str,
    value: Option<T>,
) {
    if let Some(value) = value {
        target.push((name, value.to_string()));
    }
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn dead_letter_path(queue: &str) -> String {
    format!("{}/$deadletterqueue", queue.trim_end_matches('/'))
}

fn custom_properties(headers: &HeaderMap) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (name, value) in headers {
        let name = name.as_str();
        if is_standard_header(name) {
            continue;
        }
        if let Ok(value) = value.to_str() {
            out.insert(name.to_string(), value.trim_matches('"').to_string());
        }
    }
    out
}

fn is_standard_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "authorization"
            | "brokerproperties"
            | "content-type"
            | "content-length"
            | "date"
            | "server"
            | "transfer-encoding"
            | "location"
    )
}

fn extract_xml_u64(body: &str, tag: &str) -> Option<u64> {
    let start = format!("<{tag}>");
    let end = format!("</{tag}>");
    let value = body.split(&start).nth(1)?.split(&end).next()?;
    value.trim().parse().ok()
}

fn strip_query(value: &str) -> &str {
    value.split_once('?').map(|(url, _)| url).unwrap_or(value)
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

fn percent_encode_path(value: &str) -> String {
    value
        .split('/')
        .map(percent_encode_path_segment)
        .collect::<Vec<_>>()
        .join("/")
}

fn percent_encode_path_segment(value: &str) -> String {
    percent_encode(value.as_bytes(), is_unreserved)
}

fn percent_encode_query_value(value: &str) -> String {
    percent_encode(value.as_bytes(), is_unreserved)
}

fn form_urlencoded(fields: &[(&str, &str)]) -> String {
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

pub(crate) fn properties_from_json(value: &Option<Value>) -> BTreeMap<String, String> {
    let mut properties = BTreeMap::new();
    let Some(Value::Object(map)) = value else {
        return properties;
    };
    for (key, value) in map {
        let header_name = HeaderName::from_bytes(key.as_bytes());
        if header_name.is_err() {
            continue;
        }
        let value = match value {
            Value::String(value) => value.clone(),
            other => other.to_string(),
        };
        if HeaderValue::from_str(&value).is_ok() {
            properties.insert(key.clone(), value);
        }
    }
    properties
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_connection_string_namespace_and_sas_keys() {
        let parsed = parse_connection_string(
            "Endpoint=sb://example.servicebus.windows.net/;SharedAccessKeyName=RootManageSharedAccessKey;SharedAccessKey=abc=",
        )
        .unwrap();
        assert_eq!(
            parsed.fully_qualified_namespace,
            "example.servicebus.windows.net"
        );
        assert_eq!(parsed.shared_access_key_name, "RootManageSharedAccessKey");
        assert_eq!(parsed.shared_access_key, "abc=");
    }

    #[test]
    fn sas_token_uses_encoded_resource() {
        let token = sas_token(
            "https://example.servicebus.windows.net/orders blue",
            "rule",
            "key",
            60,
        );
        assert!(token.starts_with("SharedAccessSignature sr=https%3A%2F%2Fexample"));
        assert!(token.contains("skn=rule"));
    }

    #[test]
    fn path_encoding_preserves_segments() {
        assert_eq!(
            percent_encode_path("orders/blue queue"),
            "orders/blue%20queue"
        );
    }

    #[test]
    fn dead_letter_path_uses_service_bus_subqueue_suffix() {
        assert_eq!(
            dead_letter_path("orders-blue"),
            "orders-blue/$deadletterqueue"
        );
        assert_eq!(
            dead_letter_path("orders-blue/"),
            "orders-blue/$deadletterqueue"
        );
        assert_eq!(
            percent_encode_path(&dead_letter_path("orders-blue")),
            "orders-blue/%24deadletterqueue"
        );
    }

    #[test]
    fn extracts_queue_message_count_from_atom_xml() {
        assert_eq!(
            extract_xml_u64(
                "<QueueDescription><MessageCount>17</MessageCount></QueueDescription>",
                "MessageCount"
            ),
            Some(17)
        );
    }

    #[test]
    fn queue_description_xml_contains_configured_properties() {
        let declaration = QueueDeclarationConfig {
            lock_duration: Some("PT30S".to_string()),
            max_delivery_count: Some(20),
            dead_lettering_on_message_expiration: Some(true),
            forward_dead_lettered_messages_to: Some("orders-blue_dlq".to_string()),
            ..Default::default()
        };

        let xml = queue_description_xml(&declaration);

        assert!(xml.contains("<LockDuration>PT30S</LockDuration>"));
        assert!(xml.contains("<MaxDeliveryCount>20</MaxDeliveryCount>"));
        assert!(
            xml.contains(
                "<DeadLetteringOnMessageExpiration>true</DeadLetteringOnMessageExpiration>"
            )
        );
        assert!(xml.contains(
            "<ForwardDeadLetteredMessagesTo>orders-blue_dlq</ForwardDeadLetteredMessagesTo>"
        ));
    }

    #[test]
    fn queue_description_arm_contains_configured_properties() {
        let properties = queue_description_arm_properties(&QueueDeclarationConfig {
            lock_duration: Some("PT45S".to_string()),
            max_delivery_count: Some(12),
            enable_partitioning: Some(true),
            ..Default::default()
        });

        assert_eq!(properties["lockDuration"], "PT45S");
        assert_eq!(properties["maxDeliveryCount"], 12);
        assert_eq!(properties["enablePartitioning"], true);
        assert_eq!(properties["enableBatchedOperations"], true);
    }
}
