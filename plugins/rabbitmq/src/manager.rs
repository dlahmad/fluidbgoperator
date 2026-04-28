use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use fluidbg_plugin_sdk::{
    AUTHORIZATION_HEADER, ActiveInception, InceptorEnvVar, PluginAuthClaims,
    PluginManagerLifecycleRequest, PluginManagerSyncRequest, PluginRole,
    derived_scoped_identity_name, derived_temp_queue_name_with_uid,
    require_manager_request_matches_claims, sign_plugin_auth_token, verify_manager_bearer_token,
};
use serde_json::Value;

use crate::amqp::{connect_with_retry, declare_queue, delete_queue};
use crate::assignments::build_prepare_assignments;
use crate::config::{Config, has_role, shadow_queue_name};
use crate::management::ManagementClient;

#[derive(Clone)]
pub(crate) struct ManagerState {
    pub(crate) signing_key: Vec<u8>,
    pub(crate) amqp_url: String,
    pub(crate) management_url: Option<String>,
    pub(crate) management_username: Option<String>,
    pub(crate) management_password: Option<String>,
    pub(crate) management_vhost: Option<String>,
}

pub(crate) fn manager_state_from_env() -> anyhow::Result<ManagerState> {
    let signing_key = std::env::var("FLUIDBG_MANAGER_AUTH_SIGNING_KEY")
        .map_err(|_| anyhow::anyhow!("missing FLUIDBG_MANAGER_AUTH_SIGNING_KEY"))?;
    let amqp_url = std::env::var("FLUIDBG_RABBITMQ_MANAGER_AMQP_URL")
        .map_err(|_| anyhow::anyhow!("missing FLUIDBG_RABBITMQ_MANAGER_AMQP_URL"))?;
    Ok(ManagerState {
        signing_key: signing_key.into_bytes(),
        amqp_url,
        management_url: std::env::var("FLUIDBG_RABBITMQ_MANAGER_MANAGEMENT_URL").ok(),
        management_username: std::env::var("FLUIDBG_RABBITMQ_MANAGER_MANAGEMENT_USERNAME").ok(),
        management_password: std::env::var("FLUIDBG_RABBITMQ_MANAGER_MANAGEMENT_PASSWORD").ok(),
        management_vhost: std::env::var("FLUIDBG_RABBITMQ_MANAGER_MANAGEMENT_VHOST").ok(),
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
    reconcile_queues(&state, &req.roles, &config, true).await?;
    let inceptor_env = prepare_scoped_inceptor_env(&state, &claims, &req.roles, &config).await?;
    garbage_collect_scoped_identities(&state, &req.active_inceptions).await?;
    let roles = parse_roles(&req.roles);
    Ok(Json(
        serde_json::to_value(fluidbg_plugin_sdk::PluginLifecycleResponse {
            assignments: build_prepare_assignments(&config, &roles),
            inceptor_env,
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
    reconcile_queues(&state, &req.roles, &config, false).await?;
    cleanup_scoped_inceptor_identity(&state, &claims).await?;
    garbage_collect_scoped_identities(&state, &req.active_inceptions).await?;
    Ok(Json(serde_json::json!({"ok": true})))
}

pub(crate) async fn sync_handler(
    State(state): State<ManagerState>,
    headers: HeaderMap,
    Json(req): Json<PluginManagerSyncRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let claims = authorize(&state, &headers)?;
    ensure_sync_request_matches_claims(&req, &claims)?;
    garbage_collect_scoped_identities(&state, &req.active_inceptions).await?;
    garbage_collect_temp_queues(&state, &req.active_inceptions).await?;
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

async fn prepare_scoped_inceptor_env(
    state: &ManagerState,
    claims: &PluginAuthClaims,
    roles: &[String],
    config: &Config,
) -> Result<Vec<InceptorEnvVar>, StatusCode> {
    let management = management_client(state)?;
    let username = scoped_username(claims);
    let password = sign_plugin_auth_token(claims, &state.signing_key)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let queues = inceptor_queue_names(&parse_roles(roles), config);
    management
        .create_inceptor_user(&username, &password, &queues)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(inceptor_env(
        state,
        management.base_url(),
        management.vhost(),
        &username,
        &password,
    ))
}

async fn cleanup_scoped_inceptor_identity(
    state: &ManagerState,
    claims: &PluginAuthClaims,
) -> Result<(), StatusCode> {
    let management = management_client(state)?;
    management
        .delete_user(&scoped_username(claims))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn garbage_collect_scoped_identities(
    state: &ManagerState,
    active_inceptions: &[ActiveInception],
) -> Result<(), StatusCode> {
    let management = management_client(state)?;
    let active = active_inceptions
        .iter()
        .map(scoped_username_from_active)
        .collect::<std::collections::BTreeSet<_>>();
    let users = management
        .list_users()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    for user in users {
        if user.starts_with("fbg-rmq-") && !active.contains(&user) {
            management
                .delete_user(&user)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        }
    }
    Ok(())
}

async fn garbage_collect_temp_queues(
    state: &ManagerState,
    active_inceptions: &[ActiveInception],
) -> Result<(), StatusCode> {
    let management = management_client(state)?;
    let active = active_queue_names(active_inceptions);
    let queues = management
        .list_queues()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let conn = connect_with_retry(&state.amqp_url)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let channel = conn
        .create_channel()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    for queue in queues {
        if queue.starts_with("fluidbg-") && !active.contains(&queue) {
            delete_queue(&channel, &queue)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        }
    }
    Ok(())
}

fn management_client(state: &ManagerState) -> Result<ManagementClient, StatusCode> {
    let url = state
        .management_url
        .clone()
        .filter(|value| !value.is_empty())
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
    let username = state
        .management_username
        .clone()
        .filter(|value| !value.is_empty())
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
    let password = state
        .management_password
        .clone()
        .filter(|value| !value.is_empty())
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(ManagementClient::new(
        url,
        username,
        password,
        state
            .management_vhost
            .clone()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "/".to_string()),
    ))
}

fn inceptor_env(
    state: &ManagerState,
    management_url: &str,
    management_vhost: &str,
    username: &str,
    password: &str,
) -> Vec<InceptorEnvVar> {
    let mut env = vec![InceptorEnvVar {
        name: "FLUIDBG_RABBITMQ_AMQP_URL".to_string(),
        value: amqp_url_with_credentials(&state.amqp_url, username, password),
    }];
    push_optional_env(
        &mut env,
        "FLUIDBG_RABBITMQ_MANAGEMENT_URL",
        Some(management_url),
    );
    push_optional_env(
        &mut env,
        "FLUIDBG_RABBITMQ_MANAGEMENT_USERNAME",
        Some(username),
    );
    push_optional_env(
        &mut env,
        "FLUIDBG_RABBITMQ_MANAGEMENT_PASSWORD",
        Some(password),
    );
    push_optional_env(
        &mut env,
        "FLUIDBG_RABBITMQ_MANAGEMENT_VHOST",
        Some(management_vhost),
    );
    env
}

fn scoped_username(claims: &PluginAuthClaims) -> String {
    scoped_username_parts(
        &claims.namespace,
        &claims.blue_green_ref,
        claims.blue_green_uid.as_deref().unwrap_or(""),
        &claims.inception_point,
    )
}

fn scoped_username_from_active(active: &ActiveInception) -> String {
    scoped_username_parts(
        &active.namespace,
        &active.blue_green_ref,
        active.blue_green_uid.as_deref().unwrap_or(""),
        &active.inception_point,
    )
}

fn scoped_username_parts(
    namespace: &str,
    blue_green_ref: &str,
    blue_green_uid: &str,
    inception_point: &str,
) -> String {
    derived_scoped_identity_name(
        "fbg-rmq",
        namespace,
        blue_green_ref,
        blue_green_uid,
        inception_point,
        64,
    )
}

fn amqp_url_with_credentials(base: &str, username: &str, password: &str) -> String {
    let Some(scheme_end) = base.find("://").map(|idx| idx + 3) else {
        return base.to_string();
    };
    let rest = &base[scheme_end..];
    let authority_end = rest
        .find('/')
        .map(|idx| scheme_end + idx)
        .unwrap_or(base.len());
    let authority = &base[scheme_end..authority_end];
    let host_start = authority
        .rfind('@')
        .map(|idx| scheme_end + idx + 1)
        .unwrap_or(scheme_end);
    format!(
        "{}{}:{}@{}",
        &base[..scheme_end],
        encode_userinfo(username),
        encode_userinfo(password),
        &base[host_start..]
    )
}

fn encode_userinfo(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.as_bytes() {
        match *byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(*byte as char)
            }
            byte => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
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
    serde_json::from_value(value).unwrap_or_default()
}

async fn reconcile_queues(
    state: &ManagerState,
    roles: &[String],
    config: &Config,
    create: bool,
) -> Result<(), StatusCode> {
    let roles = parse_roles(roles);
    let conn = connect_with_retry(&state.amqp_url)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let channel = conn
        .create_channel()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let queues = managed_temp_queue_names(&roles, config);
    if create && let Some(shadow) = &config.shadow_queue {
        let shadow_declaration = &shadow.queue_declaration;
        for queue in base_shadow_queue_names(&roles, config) {
            if let Some(shadow_queue) = shadow_queue_name(config, queue) {
                declare_queue(&channel, &shadow_queue, shadow_declaration)
                    .await
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            }
        }
    }
    for queue in queues {
        if create {
            if let Some(shadow_queue) = shadow_queue_name(config, &queue) {
                let shadow_declaration = config
                    .shadow_queue
                    .as_ref()
                    .map(|shadow| &shadow.queue_declaration)
                    .unwrap_or(&config.queue_declaration);
                declare_queue(&channel, &shadow_queue, shadow_declaration)
                    .await
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
                declare_queue(&channel, &queue, &config.queue_declaration)
                    .await
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            } else {
                declare_queue(&channel, &queue, &config.queue_declaration)
                    .await
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            }
        } else {
            delete_queue(&channel, &queue)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            if let Some(shadow_queue) = shadow_queue_name(config, &queue) {
                delete_queue(&channel, &shadow_queue)
                    .await
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            }
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
    set_nested_string(
        config,
        &["duplicator", "greenInputQueue"],
        derived_temp_queue_name_with_uid(
            namespace,
            blue_green_ref,
            blue_green_uid,
            inception_point,
            "duplicator",
            "green-input",
        ),
    );
    set_nested_string(
        config,
        &["duplicator", "blueInputQueue"],
        derived_temp_queue_name_with_uid(
            namespace,
            blue_green_ref,
            blue_green_uid,
            inception_point,
            "duplicator",
            "blue-input",
        ),
    );
    set_nested_string(
        config,
        &["splitter", "greenInputQueue"],
        derived_temp_queue_name_with_uid(
            namespace,
            blue_green_ref,
            blue_green_uid,
            inception_point,
            "splitter",
            "green-input",
        ),
    );
    set_nested_string(
        config,
        &["splitter", "blueInputQueue"],
        derived_temp_queue_name_with_uid(
            namespace,
            blue_green_ref,
            blue_green_uid,
            inception_point,
            "splitter",
            "blue-input",
        ),
    );
    set_nested_string(
        config,
        &["combiner", "greenOutputQueue"],
        derived_temp_queue_name_with_uid(
            namespace,
            blue_green_ref,
            blue_green_uid,
            inception_point,
            "combiner",
            "green-output",
        ),
    );
    set_nested_string(
        config,
        &["combiner", "blueOutputQueue"],
        derived_temp_queue_name_with_uid(
            namespace,
            blue_green_ref,
            blue_green_uid,
            inception_point,
            "combiner",
            "blue-output",
        ),
    );
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

    #[test]
    fn scoped_usernames_are_bounded_and_deterministic() {
        let name = scoped_username_parts(
            "very-long-namespace-name",
            "very-long-blue-green-deployment-name",
            "uid-123",
            "very-long-inception-point-name",
        );

        assert!(name.starts_with("fbg-rmq-"));
        assert!(name.len() <= 64);
        assert_eq!(
            name,
            scoped_username_parts(
                "very-long-namespace-name",
                "very-long-blue-green-deployment-name",
                "uid-123",
                "very-long-inception-point-name",
            )
        );
    }

    #[test]
    fn active_queue_inventory_uses_rewritten_temp_names() {
        let active = ActiveInception {
            namespace: "apps".to_string(),
            blue_green_ref: "orders".to_string(),
            blue_green_uid: Some("uid-1".to_string()),
            inception_point: "incoming".to_string(),
            plugin: "rabbitmq".to_string(),
            roles: vec!["duplicator".to_string()],
            config: serde_json::json!({
                "duplicator": {
                    "inputQueue": "orders",
                    "greenInputQueue": "user-provided-green",
                    "blueInputQueue": "user-provided-blue"
                },
                "shadowQueue": { "suffix": "_dlq" }
            }),
        };

        let queues = active_queue_names(&[active]);

        assert!(
            queues
                .iter()
                .any(|queue| queue.starts_with("fluidbg-green-input-"))
        );
        assert!(
            queues
                .iter()
                .any(|queue| queue.starts_with("fluidbg-blue-input-"))
        );
        assert!(queues.iter().any(|queue| queue.ends_with("_dlq")));
        assert!(!queues.contains("user-provided-green"));
    }
}
