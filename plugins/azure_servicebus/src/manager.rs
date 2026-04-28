use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use fluidbg_plugin_sdk::{
    AUTHORIZATION_HEADER, PluginAuthClaims, PluginManagerLifecycleRequest, PluginRole,
    bearer_token, derived_temp_queue_name_with_uid, verify_plugin_auth_token,
};
use serde_json::Value;

use crate::assignments::build_prepare_assignments;
use crate::config::{Config, has_role, shadow_queue_name};
use crate::servicebus::ServiceBusClient;

#[derive(Clone)]
pub(crate) struct ManagerState {
    pub(crate) signing_key: Vec<u8>,
    pub(crate) base_config: Config,
}

pub(crate) fn manager_state_from_env() -> anyhow::Result<ManagerState> {
    let signing_key = std::env::var("FLUIDBG_MANAGER_AUTH_SIGNING_KEY")
        .map_err(|_| anyhow::anyhow!("missing FLUIDBG_MANAGER_AUTH_SIGNING_KEY"))?;
    let base_config = if let Ok(path) = std::env::var("FLUIDBG_MANAGER_CONFIG_PATH") {
        let contents = std::fs::read_to_string(path)?;
        serde_yaml_ng::from_str(&contents)?
    } else {
        Config {
            connection_string: std::env::var("AZURE_SERVICEBUS_CONNECTION_STRING").ok(),
            fully_qualified_namespace: std::env::var("AZURE_SERVICEBUS_NAMESPACE").ok(),
            ..Default::default()
        }
    };
    Ok(ManagerState {
        signing_key: signing_key.into_bytes(),
        base_config,
    })
}

pub(crate) async fn prepare_handler(
    State(state): State<ManagerState>,
    headers: HeaderMap,
    Json(req): Json<PluginManagerLifecycleRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let claims = authorize(&state, &headers)?;
    let config = secured_config_from_claims(&state, &claims, &req);
    reconcile_queues(&req.roles, &config, true).await?;
    let roles = parse_roles(&req.roles);
    Ok(Json(
        serde_json::to_value(fluidbg_plugin_sdk::PluginLifecycleResponse {
            assignments: build_prepare_assignments(&config, &roles),
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
    let config = secured_config_from_claims(&state, &claims, &req);
    reconcile_queues(&req.roles, &config, false).await?;
    Ok(Json(serde_json::json!({"ok": true})))
}

pub(crate) async fn health() -> &'static str {
    "ok"
}

fn authorize(state: &ManagerState, headers: &HeaderMap) -> Result<PluginAuthClaims, StatusCode> {
    let header = headers
        .get(AUTHORIZATION_HEADER)
        .and_then(|value| value.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let token = bearer_token(header).ok_or(StatusCode::UNAUTHORIZED)?;
    verify_plugin_auth_token(token, &state.signing_key).map_err(|_| StatusCode::UNAUTHORIZED)
}

fn secured_config_from_claims(
    state: &ManagerState,
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
    let mut config = serde_json::from_value::<Config>(value).unwrap_or_default();
    config.connection_string = state.base_config.connection_string.clone();
    config.fully_qualified_namespace = state.base_config.fully_qualified_namespace.clone();
    config.auth = state.base_config.auth.clone();
    config.management = state.base_config.management.clone();
    config
}

async fn reconcile_queues(
    roles: &[String],
    config: &Config,
    create: bool,
) -> Result<(), StatusCode> {
    let roles = parse_roles(roles);
    let client = ServiceBusClient::from_config(config).map_err(|_| StatusCode::BAD_REQUEST)?;
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
