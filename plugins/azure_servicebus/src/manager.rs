use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use fluidbg_plugin_sdk::{
    AUTHORIZATION_HEADER, ActiveInception, InceptorEnvVar, PluginAuthClaims,
    PluginManagerLifecycleRequest, PluginManagerSyncRequest, PluginRole,
    require_manager_request_matches_claims, verify_manager_bearer_token,
};
use serde_json::Value;

use crate::assignments::build_prepare_assignments;
use crate::config::{Config, ServiceBusRuntimeConfig, has_role, shadow_queue_name};
use crate::servicebus::{
    ServiceBusClient, parse_connection_string, sas_token, service_bus_resource_url,
};

#[derive(Clone)]
pub(crate) struct ManagerState {
    pub(crate) signing_key: Vec<u8>,
    pub(crate) runtime_config: ServiceBusRuntimeConfig,
}

pub(crate) fn manager_state_from_env() -> anyhow::Result<ManagerState> {
    let signing_key = std::env::var("FLUIDBG_MANAGER_AUTH_SIGNING_KEY")
        .map_err(|_| anyhow::anyhow!("missing FLUIDBG_MANAGER_AUTH_SIGNING_KEY"))?;
    Ok(ManagerState {
        signing_key: signing_key.into_bytes(),
        runtime_config: ServiceBusRuntimeConfig::from_env(),
    })
}

pub(crate) async fn prepare_handler(
    State(state): State<ManagerState>,
    headers: HeaderMap,
    Json(req): Json<PluginManagerLifecycleRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let claims = authorize(&state, &headers)?;
    ensure_request_matches_claims(&req, &claims)?;
    let config = secured_config_from_claims(&claims, &req);
    reconcile_queues(&state.runtime_config, &req.roles, &config, true).await?;
    let roles = parse_roles(&req.roles);
    Ok(Json(
        serde_json::to_value(fluidbg_plugin_sdk::PluginLifecycleResponse {
            assignments: build_prepare_assignments(&config, &roles),
            inceptor_env: inceptor_env(&state.runtime_config, &roles, &config),
        })
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
    ))
}

pub(crate) async fn cleanup_handler(
    State(state): State<ManagerState>,
    headers: HeaderMap,
    Json(req): Json<PluginManagerLifecycleRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let claims = authorize(&state, &headers)?;
    ensure_request_matches_claims(&req, &claims)?;
    let config = secured_config_from_claims(&claims, &req);
    reconcile_queues(&state.runtime_config, &req.roles, &config, false).await?;
    Ok(Json(serde_json::json!({"ok": true})))
}

pub(crate) async fn sync_handler(
    State(state): State<ManagerState>,
    headers: HeaderMap,
    Json(req): Json<PluginManagerSyncRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let claims = authorize(&state, &headers)?;
    ensure_sync_request_matches_claims(&req, &claims)?;
    garbage_collect_temp_queues(&state.runtime_config, &req.active_inceptions).await?;
    Ok(Json(serde_json::json!({"ok": true})))
}

pub(crate) async fn health() -> &'static str {
    "ok"
}

fn authorize(state: &ManagerState, headers: &HeaderMap) -> Result<PluginAuthClaims, StatusCode> {
    let header = headers
        .get(AUTHORIZATION_HEADER)
        .and_then(|value| value.to_str().ok());
    verify_manager_bearer_token(header, &state.signing_key).map_err(|_| StatusCode::UNAUTHORIZED)
}

fn ensure_request_matches_claims(
    req: &PluginManagerLifecycleRequest,
    claims: &PluginAuthClaims,
) -> Result<(), StatusCode> {
    require_manager_request_matches_claims(req, claims).map_err(|_| StatusCode::UNAUTHORIZED)
}

fn ensure_sync_request_matches_claims(
    req: &PluginManagerSyncRequest,
    claims: &PluginAuthClaims,
) -> Result<(), StatusCode> {
    if claims.blue_green_ref == "__manager_sync__"
        && claims.inception_point == "__manager_sync__"
        && claims.plugin == req.plugin
    {
        Ok(())
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

fn inceptor_env(
    runtime: &ServiceBusRuntimeConfig,
    roles: &[PluginRole],
    config: &Config,
) -> Vec<InceptorEnvVar> {
    let mut env = Vec::new();
    let connection = runtime
        .connection_string
        .as_deref()
        .and_then(|value| parse_connection_string(value).ok());
    let namespace = runtime.fully_qualified_namespace.as_deref().or_else(|| {
        connection
            .as_ref()
            .map(|parsed| parsed.fully_qualified_namespace.as_str())
    });
    push_optional_env(&mut env, "AZURE_SERVICEBUS_NAMESPACE", namespace);
    if let Some(tokens_json) = static_sas_tokens_json(runtime, roles, config) {
        push_optional_env(
            &mut env,
            "FLUIDBG_AZURE_SERVICEBUS_SAS_TOKENS_JSON",
            Some(&tokens_json),
        );
    }
    if let Some(auth) = &runtime.auth
        && let Some(mode) = auth.mode
    {
        let value = match mode {
            crate::config::AuthMode::ConnectionString => "connectionString",
            crate::config::AuthMode::WorkloadIdentity => "workloadIdentity",
        };
        push_optional_env(&mut env, "FLUIDBG_AZURE_SERVICEBUS_AUTH_MODE", Some(value));
    }
    env
}

fn static_sas_tokens_json(
    runtime: &ServiceBusRuntimeConfig,
    roles: &[PluginRole],
    config: &Config,
) -> Option<String> {
    let connection = runtime
        .connection_string
        .as_deref()
        .and_then(|value| parse_connection_string(value).ok())?;
    let ttl = runtime
        .auth
        .as_ref()
        .and_then(|auth| auth.sas_token_ttl_seconds)
        .unwrap_or(3600);
    let mut tokens = std::collections::BTreeMap::new();
    for queue in inceptor_queue_names(roles, config) {
        let resource = service_bus_resource_url(&connection.fully_qualified_namespace, &queue);
        tokens.insert(
            resource.clone(),
            sas_token(
                &resource,
                &connection.shared_access_key_name,
                &connection.shared_access_key,
                ttl,
            ),
        );
    }
    serde_json::to_string(&tokens).ok()
}

fn push_optional_env(env: &mut Vec<InceptorEnvVar>, name: &str, value: Option<&str>) {
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        env.push(InceptorEnvVar {
            name: name.to_string(),
            value: value.to_string(),
        });
    }
}

fn secured_config_from_claims(
    claims: &PluginAuthClaims,
    req: &PluginManagerLifecycleRequest,
) -> Config {
    let mut value = req.config.clone();
    rewrite_queue_temp_names(
        &mut value,
        &claims.namespace,
        &claims.blue_green_ref,
        claims.blue_green_uid.as_deref().unwrap_or(""),
        &claims.inception_point,
    );
    serde_json::from_value::<Config>(value).unwrap_or_default()
}

async fn reconcile_queues(
    runtime_config: &ServiceBusRuntimeConfig,
    roles: &[String],
    config: &Config,
    create: bool,
) -> Result<(), StatusCode> {
    let roles = parse_roles(roles);
    let client = ServiceBusClient::from_runtime_config(runtime_config)
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let mut queues = Vec::new();
    if has_role(&roles, PluginRole::Duplicator)
        && let Some(duplicator) = &config.duplicator
    {
        queues.extend([
            duplicator.green_input_queue.as_deref(),
            duplicator.blue_input_queue.as_deref(),
        ]);
    }
    if has_role(&roles, PluginRole::Splitter)
        && let Some(splitter) = &config.splitter
    {
        queues.extend([
            splitter.green_input_queue.as_deref(),
            splitter.blue_input_queue.as_deref(),
        ]);
    }
    if has_role(&roles, PluginRole::Combiner)
        && let Some(combiner) = &config.combiner
    {
        queues.extend([
            combiner.green_output_queue.as_deref(),
            combiner.blue_output_queue.as_deref(),
        ]);
    }
    if create && let Some(shadow) = &config.shadow_queue {
        let shadow_declaration = &shadow.queue_declaration;
        let mut target_shadow_queues = Vec::new();
        if has_role(&roles, PluginRole::Duplicator)
            && let Some(duplicator) = &config.duplicator
            && let Some(input_queue) = duplicator.input_queue.as_deref()
        {
            target_shadow_queues.push(input_queue);
        }
        if has_role(&roles, PluginRole::Splitter)
            && let Some(splitter) = &config.splitter
            && let Some(input_queue) = splitter.input_queue.as_deref()
        {
            target_shadow_queues.push(input_queue);
        }
        if has_role(&roles, PluginRole::Combiner)
            && let Some(combiner) = &config.combiner
            && let Some(output_queue) = combiner.output_queue.as_deref()
        {
            target_shadow_queues.push(output_queue);
        }
        for queue in target_shadow_queues {
            if let Some(shadow_queue) = shadow_queue_name(config, queue) {
                client
                    .create_queue(&shadow_queue, shadow_declaration)
                    .await
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            }
        }
    }
    for queue in queues.into_iter().flatten() {
        let result = if create {
            if let Some(shadow_queue) = shadow_queue_name(config, queue) {
                let shadow_declaration = config
                    .shadow_queue
                    .as_ref()
                    .map(|shadow| &shadow.queue_declaration)
                    .unwrap_or(&config.queue_declaration);
                client
                    .create_queue(&shadow_queue, shadow_declaration)
                    .await
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
                client.create_queue(queue, &config.queue_declaration).await
            } else {
                client.create_queue(queue, &config.queue_declaration).await
            }
        } else {
            client
                .delete_queue(queue)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            if let Some(shadow_queue) = shadow_queue_name(config, queue) {
                client
                    .delete_queue(&shadow_queue)
                    .await
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            }
            Ok(())
        };
        result.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    Ok(())
}

async fn garbage_collect_temp_queues(
    runtime_config: &ServiceBusRuntimeConfig,
    active_inceptions: &[ActiveInception],
) -> Result<(), StatusCode> {
    let client = ServiceBusClient::from_runtime_config(runtime_config)
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let active = active_queue_names(active_inceptions);
    let queues = client
        .list_queues()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    for queue in queues {
        if queue.starts_with("fluidbg-") && !active.contains(&queue) {
            client
                .delete_queue(&queue)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        }
    }
    Ok(())
}

fn managed_temp_queue_names(roles: &[PluginRole], config: &Config) -> Vec<String> {
    let mut queues = Vec::new();
    if has_role(roles, PluginRole::Duplicator)
        && let Some(duplicator) = &config.duplicator
    {
        queues.extend([
            duplicator.green_input_queue.clone(),
            duplicator.blue_input_queue.clone(),
        ]);
    }
    if has_role(roles, PluginRole::Splitter)
        && let Some(splitter) = &config.splitter
    {
        queues.extend([
            splitter.green_input_queue.clone(),
            splitter.blue_input_queue.clone(),
        ]);
    }
    if has_role(roles, PluginRole::Combiner)
        && let Some(combiner) = &config.combiner
    {
        queues.extend([
            combiner.green_output_queue.clone(),
            combiner.blue_output_queue.clone(),
        ]);
    }
    queues.into_iter().flatten().collect()
}

fn base_shadow_queue_names<'a>(roles: &[PluginRole], config: &'a Config) -> Vec<&'a str> {
    let mut queues = Vec::new();
    if has_role(roles, PluginRole::Duplicator)
        && let Some(duplicator) = &config.duplicator
        && let Some(input_queue) = duplicator.input_queue.as_deref()
    {
        queues.push(input_queue);
    }
    if has_role(roles, PluginRole::Splitter)
        && let Some(splitter) = &config.splitter
        && let Some(input_queue) = splitter.input_queue.as_deref()
    {
        queues.push(input_queue);
    }
    if has_role(roles, PluginRole::Combiner)
        && let Some(combiner) = &config.combiner
        && let Some(output_queue) = combiner.output_queue.as_deref()
    {
        queues.push(output_queue);
    }
    queues
}

fn inceptor_queue_names(roles: &[PluginRole], config: &Config) -> Vec<String> {
    let mut queues = managed_temp_queue_names(roles, config);
    for queue in base_shadow_queue_names(roles, config) {
        queues.push(queue.to_string());
        if let Some(shadow_queue) = shadow_queue_name(config, queue) {
            queues.push(shadow_queue);
        }
    }
    if has_role(roles, PluginRole::Writer)
        && let Some(writer) = &config.writer
        && let Some(queue) = &writer.target_queue
    {
        queues.push(queue.clone());
    }
    if has_role(roles, PluginRole::Consumer)
        && let Some(consumer) = &config.consumer
        && let Some(queue) = &consumer.input_queue
    {
        queues.push(queue.clone());
    }
    let temp_shadows = queues
        .iter()
        .filter_map(|queue| shadow_queue_name(config, queue))
        .collect::<Vec<_>>();
    queues.extend(temp_shadows);
    queues.sort();
    queues.dedup();
    queues
}

fn active_queue_names(active_inceptions: &[ActiveInception]) -> std::collections::BTreeSet<String> {
    let mut queues = std::collections::BTreeSet::new();
    for active in active_inceptions {
        let mut value = active.config.clone();
        rewrite_queue_temp_names(
            &mut value,
            &active.namespace,
            &active.blue_green_ref,
            active.blue_green_uid.as_deref().unwrap_or(""),
            &active.inception_point,
        );
        let config = serde_json::from_value::<Config>(value).unwrap_or_default();
        let roles = parse_roles(&active.roles);
        let mut local = managed_temp_queue_names(&roles, &config);
        let shadow_queues = local
            .iter()
            .filter_map(|queue| shadow_queue_name(&config, queue))
            .collect::<Vec<_>>();
        local.extend(shadow_queues);
        queues.extend(local);
    }
    queues
}

fn parse_roles(roles: &[String]) -> Vec<PluginRole> {
    roles
        .iter()
        .filter_map(|role| PluginRole::parse(role))
        .collect()
}

fn rewrite_queue_temp_names(
    config: &mut Value,
    namespace: &str,
    blue_green_ref: &str,
    blue_green_uid: &str,
    inception_point: &str,
) {
    let duplicator_identifier = temporary_queue_identifier(config, "duplicator");
    let splitter_identifier = temporary_queue_identifier(config, "splitter");
    let combiner_identifier = temporary_queue_identifier(config, "combiner");
    set_nested_string(
        config,
        &["duplicator", "greenInputQueue"],
        fluidbg_plugin_sdk::derived_temp_queue_name_with_uid_and_identifier(
            namespace,
            blue_green_ref,
            blue_green_uid,
            inception_point,
            "duplicator",
            "green-input",
            duplicator_identifier.as_deref(),
        ),
    );
    set_nested_string(
        config,
        &["duplicator", "blueInputQueue"],
        fluidbg_plugin_sdk::derived_temp_queue_name_with_uid_and_identifier(
            namespace,
            blue_green_ref,
            blue_green_uid,
            inception_point,
            "duplicator",
            "blue-input",
            duplicator_identifier.as_deref(),
        ),
    );
    set_nested_string(
        config,
        &["splitter", "greenInputQueue"],
        fluidbg_plugin_sdk::derived_temp_queue_name_with_uid_and_identifier(
            namespace,
            blue_green_ref,
            blue_green_uid,
            inception_point,
            "splitter",
            "green-input",
            splitter_identifier.as_deref(),
        ),
    );
    set_nested_string(
        config,
        &["splitter", "blueInputQueue"],
        fluidbg_plugin_sdk::derived_temp_queue_name_with_uid_and_identifier(
            namespace,
            blue_green_ref,
            blue_green_uid,
            inception_point,
            "splitter",
            "blue-input",
            splitter_identifier.as_deref(),
        ),
    );
    set_nested_string(
        config,
        &["combiner", "greenOutputQueue"],
        fluidbg_plugin_sdk::derived_temp_queue_name_with_uid_and_identifier(
            namespace,
            blue_green_ref,
            blue_green_uid,
            inception_point,
            "combiner",
            "green-output",
            combiner_identifier.as_deref(),
        ),
    );
    set_nested_string(
        config,
        &["combiner", "blueOutputQueue"],
        fluidbg_plugin_sdk::derived_temp_queue_name_with_uid_and_identifier(
            namespace,
            blue_green_ref,
            blue_green_uid,
            inception_point,
            "combiner",
            "blue-output",
            combiner_identifier.as_deref(),
        ),
    );
}

fn temporary_queue_identifier(config: &Value, role: &str) -> Option<String> {
    config
        .get(role)
        .and_then(|role| role.get("temporaryQueueIdentifier"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

fn set_nested_string(config: &mut Value, path: &[&str], value: String) {
    let mut current = config;
    for segment in &path[..path.len().saturating_sub(1)] {
        let Some(next) = current.get_mut(*segment) else {
            return;
        };
        current = next;
    }
    if let Some(last) = path.last()
        && let Some(obj) = current.as_object_mut()
    {
        obj.insert((*last).to_string(), Value::String(value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AuthConfig, AuthMode, ServiceBusRuntimeConfig};

    #[test]
    fn connection_string_manager_returns_sas_tokens_not_connection_string() {
        let runtime = ServiceBusRuntimeConfig {
            connection_string: Some(
                "Endpoint=sb://example.servicebus.windows.net/;SharedAccessKeyName=RootManageSharedAccessKey;SharedAccessKey=secret"
                    .to_string(),
            ),
            fully_qualified_namespace: None,
            auth: Some(AuthConfig {
                mode: Some(AuthMode::ConnectionString),
                tenant_id: None,
                client_id: None,
                federated_token_file: None,
                authority_host: None,
                sas_token_ttl_seconds: Some(60),
            }),
            management: None,
            static_sas_tokens: Default::default(),
        };
        let config = serde_json::from_value::<Config>(serde_json::json!({
            "writer": { "targetQueue": "orders-writer" }
        }))
        .unwrap();

        let env = inceptor_env(&runtime, &[PluginRole::Writer], &config);

        assert!(
            env.iter()
                .any(|var| var.name == "AZURE_SERVICEBUS_NAMESPACE"
                    && var.value == "example.servicebus.windows.net")
        );
        assert!(
            env.iter()
                .any(|var| var.name == "FLUIDBG_AZURE_SERVICEBUS_SAS_TOKENS_JSON")
        );
        assert!(
            !env.iter()
                .any(|var| var.name == "AZURE_SERVICEBUS_CONNECTION_STRING")
        );
    }

    #[test]
    fn active_queue_inventory_uses_rewritten_temp_names() {
        let active = ActiveInception {
            namespace: "apps".to_string(),
            blue_green_ref: "orders".to_string(),
            blue_green_uid: Some("uid-1".to_string()),
            inception_point: "incoming".to_string(),
            plugin: "azure-servicebus".to_string(),
            roles: vec!["splitter".to_string()],
            config: serde_json::json!({
                "splitter": {
                    "inputQueue": "orders",
                    "greenInputQueue": "user-provided-green",
                    "blueInputQueue": "user-provided-blue",
                    "temporaryQueueIdentifier": "incoming-orders"
                }
            }),
        };

        let queues = active_queue_names(&[active]);

        assert!(
            queues
                .iter()
                .any(|queue| queue.starts_with("fluidbg-green-in-incomiada9-"))
        );
        assert!(
            queues
                .iter()
                .any(|queue| queue.starts_with("fluidbg-blue-in-incomiada9-"))
        );
        assert!(!queues.contains("user-provided-green"));
    }
}
