use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use fluidbg_plugin_sdk::{
    AUTHORIZATION_HEADER, ActiveInception, InceptorEnvVar, PluginAuthClaims,
    PluginManagerLifecycleRequest, PluginManagerSyncRequest, PluginRole, queue_worker_role,
    require_manager_request_matches_claims, verify_manager_bearer_token,
};
use serde_json::Value;

use crate::config::{Config, has_role};
use crate::nats::{NatsClient, stream_name};

#[derive(Clone)]
pub(crate) struct ManagerState {
    pub(crate) signing_key: Vec<u8>,
    pub(crate) nats_url: String,
    pub(crate) inceptor_url: String,
}

pub(crate) fn manager_state_from_env() -> anyhow::Result<ManagerState> {
    let signing_key = std::env::var("FLUIDBG_MANAGER_AUTH_SIGNING_KEY")
        .map_err(|_| anyhow::anyhow!("missing FLUIDBG_MANAGER_AUTH_SIGNING_KEY"))?;
    let nats_url = std::env::var("FLUIDBG_NATS_MANAGER_URL")
        .map_err(|_| anyhow::anyhow!("missing FLUIDBG_NATS_MANAGER_URL"))?;
    let inceptor_url = std::env::var("FLUIDBG_NATS_INCEPTOR_URL")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| nats_url.clone());
    Ok(ManagerState {
        signing_key: signing_key.into_bytes(),
        nats_url,
        inceptor_url,
    })
}

pub(crate) async fn prepare_handler(
    State(state): State<ManagerState>,
    headers: HeaderMap,
    Json(req): Json<PluginManagerLifecycleRequest>,
) -> Result<Json<Value>, StatusCode> {
    let claims = authorize(&state, &headers)?;
    require_manager_request_matches_claims(&req, &claims).map_err(|_| StatusCode::UNAUTHORIZED)?;
    let roles = parse_roles(&req.roles);
    queue_worker_role(&roles).map_err(|_| StatusCode::BAD_REQUEST)?;
    let (effective_config, config) = secured_config_from_claims(&claims, &req);
    reconcile_streams(&state, &roles, &config, true).await?;
    garbage_collect_temp_streams(&state, &req.active_inceptions).await?;
    Ok(Json(
        serde_json::to_value(fluidbg_plugin_sdk::PluginLifecycleResponse {
            assignments: crate::assignments::build_prepare_assignments(&config, &roles),
            inceptor_env: vec![InceptorEnvVar {
                name: "FLUIDBG_NATS_URL".to_string(),
                value: state.inceptor_url.clone(),
            }],
            config: Some(effective_config),
        })
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
    ))
}

pub(crate) async fn cleanup_handler(
    State(state): State<ManagerState>,
    headers: HeaderMap,
    Json(req): Json<PluginManagerLifecycleRequest>,
) -> Result<Json<Value>, StatusCode> {
    let claims = authorize(&state, &headers)?;
    require_manager_request_matches_claims(&req, &claims).map_err(|_| StatusCode::UNAUTHORIZED)?;
    let (_, config) = secured_config_from_claims(&claims, &req);
    reconcile_streams(&state, &parse_roles(&req.roles), &config, false).await?;
    garbage_collect_temp_streams(&state, &req.active_inceptions).await?;
    Ok(Json(serde_json::json!({"ok": true})))
}

pub(crate) async fn sync_handler(
    State(state): State<ManagerState>,
    headers: HeaderMap,
    Json(req): Json<PluginManagerSyncRequest>,
) -> Result<Json<Value>, StatusCode> {
    let claims = authorize(&state, &headers)?;
    if claims.blue_green_ref != "__manager_sync__"
        || claims.inception_point != "__manager_sync__"
        || claims.plugin != req.plugin
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    garbage_collect_temp_streams(&state, &req.active_inceptions).await?;
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

async fn reconcile_streams(
    state: &ManagerState,
    roles: &[PluginRole],
    config: &Config,
    create: bool,
) -> Result<(), StatusCode> {
    if config.mode.is_core() {
        return Ok(());
    }
    let client = NatsClient::connect(&state.nats_url)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let subjects = if create {
        managed_subjects(roles, config)
    } else {
        temporary_subjects(roles, config)
    };
    for subject in subjects {
        if create {
            client
                .ensure_subject_stream(&subject, &config.stream)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        } else {
            client
                .delete_subject_stream(&subject)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        }
    }
    Ok(())
}

async fn garbage_collect_temp_streams(
    state: &ManagerState,
    active_inceptions: &[ActiveInception],
) -> Result<(), StatusCode> {
    let active = active_stream_names(active_inceptions);
    let client = NatsClient::connect(&state.nats_url)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let streams = client
        .list_stream_names()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    for stream in streams {
        if stream.starts_with("fbg_fluidbg") && !active.contains(&stream) {
            let _ = client.delete_stream_name(&stream).await;
        }
    }
    Ok(())
}

fn active_stream_names(
    active_inceptions: &[ActiveInception],
) -> std::collections::BTreeSet<String> {
    let mut streams = std::collections::BTreeSet::new();
    for active in active_inceptions {
        let mut value = active.config.clone();
        rewrite_temp_subjects(
            &mut value,
            &active.namespace,
            &active.blue_green_ref,
            active.blue_green_uid.as_deref().unwrap_or(""),
            &active.inception_point,
        );
        if let Ok(config) = serde_json::from_value::<Config>(value) {
            if config.mode.is_core() {
                continue;
            }
            for subject in temporary_subjects(&parse_roles(&active.roles), &config) {
                streams.insert(stream_name(&subject));
            }
        }
    }
    streams
}

fn temporary_subjects(roles: &[PluginRole], config: &Config) -> Vec<String> {
    let mut subjects = Vec::new();
    if has_role(roles, PluginRole::Duplicator)
        && let Some(duplicator) = &config.duplicator
    {
        subjects.extend([
            duplicator.green_input_subject.clone(),
            duplicator.blue_input_subject.clone(),
        ]);
    }
    if has_role(roles, PluginRole::Splitter)
        && let Some(splitter) = &config.splitter
    {
        subjects.extend([
            splitter.green_input_subject.clone(),
            splitter.blue_input_subject.clone(),
        ]);
    }
    if has_role(roles, PluginRole::Combiner)
        && let Some(combiner) = &config.combiner
    {
        subjects.extend([
            combiner.green_output_subject.clone(),
            combiner.blue_output_subject.clone(),
        ]);
    }
    subjects.into_iter().flatten().collect()
}

fn managed_subjects(roles: &[PluginRole], config: &Config) -> Vec<String> {
    let mut subjects = Vec::new();
    if has_role(roles, PluginRole::Duplicator)
        && let Some(duplicator) = &config.duplicator
    {
        subjects.extend([
            duplicator.input_subject.clone(),
            duplicator.green_input_subject.clone(),
            duplicator.blue_input_subject.clone(),
        ]);
    }
    if has_role(roles, PluginRole::Splitter)
        && let Some(splitter) = &config.splitter
    {
        subjects.extend([
            splitter.input_subject.clone(),
            splitter.green_input_subject.clone(),
            splitter.blue_input_subject.clone(),
        ]);
    }
    if has_role(roles, PluginRole::Combiner)
        && let Some(combiner) = &config.combiner
    {
        subjects.extend([
            combiner.output_subject.clone(),
            combiner.green_output_subject.clone(),
            combiner.blue_output_subject.clone(),
        ]);
    }
    if has_role(roles, PluginRole::Writer)
        && let Some(writer) = &config.writer
    {
        subjects.push(writer.target_subject.clone());
    }
    if has_role(roles, PluginRole::Consumer)
        && let Some(consumer) = &config.consumer
    {
        subjects.push(consumer.input_subject.clone());
    }
    subjects.into_iter().flatten().collect()
}

fn secured_config_from_claims(
    claims: &PluginAuthClaims,
    req: &PluginManagerLifecycleRequest,
) -> (Value, Config) {
    let mut value = req.config.clone();
    rewrite_temp_subjects(
        &mut value,
        &claims.namespace,
        &claims.blue_green_ref,
        claims.blue_green_uid.as_deref().unwrap_or(""),
        &claims.inception_point,
    );
    let config = serde_json::from_value(value.clone()).unwrap_or_default();
    (value, config)
}

fn rewrite_temp_subjects(
    config: &mut Value,
    namespace: &str,
    blue_green_ref: &str,
    blue_green_uid: &str,
    inception_point: &str,
) {
    let duplicator_identifier = temporary_subject_identifier(config, "duplicator");
    let splitter_identifier = temporary_subject_identifier(config, "splitter");
    let combiner_identifier = temporary_subject_identifier(config, "combiner");
    set_nested_string(
        config,
        &["duplicator", "greenInputSubject"],
        temp_subject(
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
        &["duplicator", "blueInputSubject"],
        temp_subject(
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
        &["splitter", "greenInputSubject"],
        temp_subject(
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
        &["splitter", "blueInputSubject"],
        temp_subject(
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
        &["combiner", "greenOutputSubject"],
        temp_subject(
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
        &["combiner", "blueOutputSubject"],
        temp_subject(
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

fn temp_subject(
    namespace: &str,
    blue_green_ref: &str,
    blue_green_uid: &str,
    inception_point: &str,
    role: &str,
    suffix: &str,
    identifier: Option<&str>,
) -> String {
    fluidbg_plugin_sdk::derived_temp_queue_name_with_uid_and_identifier(
        namespace,
        blue_green_ref,
        blue_green_uid,
        inception_point,
        role,
        suffix,
        identifier,
    )
}

fn temporary_subject_identifier(config: &Value, role: &str) -> Option<String> {
    config
        .get(role)
        .and_then(|role| role.get("temporarySubjectIdentifier"))
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

fn parse_roles(roles: &[String]) -> Vec<PluginRole> {
    roles
        .iter()
        .filter_map(|role| PluginRole::parse(role))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_temp_subjects_with_identifier() {
        let mut config = serde_json::json!({
            "splitter": {
                "inputSubject": "orders",
                "greenInputSubject": "ignored",
                "blueInputSubject": "ignored",
                "temporarySubjectIdentifier": "incoming-orders"
            }
        });
        rewrite_temp_subjects(&mut config, "demo", "orders", "uid", "incoming");
        let green = config["splitter"]["greenInputSubject"].as_str().unwrap();
        assert!(green.starts_with("fluidbg-green-in-incom"));
        assert!(green.contains("incomi"));
        assert!(green.len() <= 63);
    }

    #[test]
    fn cleanup_subjects_exclude_base_subjects() {
        let roles = vec![PluginRole::Splitter, PluginRole::Observer];
        let mut config = serde_json::json!({
            "splitter": {
                "inputSubject": "orders",
                "greenInputSubject": "ignored",
                "blueInputSubject": "ignored",
                "temporarySubjectIdentifier": "orders-in"
            },
            "observer": {
                "notifyPath": "/observe/{testId}/orders"
            }
        });
        rewrite_temp_subjects(&mut config, "demo", "orders", "uid", "incoming");
        let config = serde_json::from_value::<Config>(config).unwrap();

        let managed = managed_subjects(&roles, &config);
        let temporary = temporary_subjects(&roles, &config);

        assert!(managed.contains(&"orders".to_string()));
        assert!(!temporary.contains(&"orders".to_string()));
        assert_eq!(temporary.len(), 2);
        assert!(
            temporary
                .iter()
                .all(|subject| subject.starts_with("fluidbg-"))
        );
    }
}
