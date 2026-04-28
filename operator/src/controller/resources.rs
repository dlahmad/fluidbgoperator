use std::collections::{BTreeMap, BTreeSet};

use k8s_openapi::api::apps::v1::{Deployment, DeploymentSpec};
use k8s_openapi::api::core::v1::{
    ConfigMap, Endpoints, EnvVar, Namespace, Pod, Secret, Service, ServiceSpec,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::ResourceExt;
use kube::api::{Api, DeleteParams, ListParams, Patch, PatchParams, PostParams};
use kube::api::{ApiResource, DynamicObject, GroupVersionKind};
use kube::core::NamespaceResourceScope;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tracing::info;

use crate::crd::blue_green::{
    BlueGreenDeployment, InceptionPoint, ManagedDeploymentSpec, TestSpec,
};
use crate::crd::inception_plugin::InceptionPlugin;
use crate::plugins::reconciler::{
    inception_auth_secret_name, inception_config_map_name, inception_instance_base_name,
    inception_service_name,
};

use super::deployments::DeploymentIdentity;
use super::plugin_lifecycle::{AssignmentKind, AssignmentTarget, PropertyAssignment};
use super::{
    AuthConfig, ReconcileError, apply_family_labels_to_deployment, apply_rollout_candidate_labels,
    blue_green_state_key, candidate_ref, clear_rollout_candidate_labels, deployment_namespace_spec,
    set_green_label,
};

pub(super) async fn ensure_test_resources(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
    assignments: &[PropertyAssignment],
) -> std::result::Result<Vec<DeploymentIdentity>, ReconcileError> {
    let mut deployments = Vec::new();
    for test in &bgd.spec.tests {
        let test_name = test_instance_name(bgd, &test.name);
        let labels = test_labels(bgd, &test.name, &test_name);
        ensure_test_name_available(bgd, client, namespace, &test.name, &test_name, &labels).await?;
        let mut deployment_spec = deployment_spec_for_test(test, &labels);
        apply_assignments_to_deployment_spec(
            &mut deployment_spec,
            assignments,
            AssignmentTarget::Test,
        )?;

        let deployment = Deployment {
            metadata: ObjectMeta {
                name: Some(test_name.clone()),
                namespace: Some(namespace.to_string()),
                labels: Some(labels.clone()),
                ..Default::default()
            },
            spec: Some(deployment_spec),
            ..Default::default()
        };
        apply_resource(
            Api::namespaced(client.clone(), namespace),
            &test_name,
            &deployment,
        )
        .await?;
        deployments.push(DeploymentIdentity {
            namespace: namespace.to_string(),
            name: test_name.clone(),
        });

        let service_spec = service_spec_for_test(test, &labels)?;
        let service = Service {
            metadata: ObjectMeta {
                name: Some(test_name.clone()),
                namespace: Some(namespace.to_string()),
                labels: Some(labels.clone()),
                ..Default::default()
            },
            spec: Some(service_spec),
            ..Default::default()
        };
        apply_resource(
            Api::namespaced(client.clone(), namespace),
            &test_name,
            &service,
        )
        .await?;
    }

    Ok(deployments)
}

fn apply_assignments_to_deployment_spec(
    deployment: &mut DeploymentSpec,
    assignments: &[PropertyAssignment],
    target: AssignmentTarget,
) -> std::result::Result<(), ReconcileError> {
    let pod_spec =
        deployment.template.spec.as_mut().ok_or_else(|| {
            ReconcileError::Store("deployment has no pod template spec".to_string())
        })?;
    for assignment in assignments {
        if assignment.target != target || !matches!(assignment.kind, AssignmentKind::Env) {
            continue;
        }
        for container in &mut pod_spec.containers {
            if let Some(target_name) = assignment.container_name.as_deref()
                && container.name != target_name
            {
                continue;
            }
            upsert_env_var(
                container.env.get_or_insert_with(Vec::new),
                &assignment.name,
                &assignment.value,
            );
        }
    }
    Ok(())
}

fn upsert_env_var(env: &mut Vec<EnvVar>, name: &str, value: &str) {
    if let Some(existing) = env.iter_mut().find(|env| env.name == name) {
        existing.value = Some(value.to_string());
        return;
    }
    env.push(EnvVar {
        name: name.to_string(),
        value: Some(value.to_string()),
        ..Default::default()
    });
}

pub(super) fn test_service_port(test: &TestSpec) -> std::result::Result<i32, ReconcileError> {
    test.service
        .ports
        .as_ref()
        .and_then(|ports| ports.first())
        .map(|port| port.port)
        .ok_or_else(|| {
            ReconcileError::Store(format!(
                "test '{}' service spec must define at least one port",
                test.name
            ))
        })
}

pub(super) async fn wait_for_test_services_ready(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
) -> std::result::Result<(), ReconcileError> {
    for test in &bgd.spec.tests {
        let service_name = test_instance_name(bgd, &test.name);
        wait_for_service_ready_endpoint(client, namespace, &service_name).await?;
    }
    Ok(())
}

async fn wait_for_service_ready_endpoint(
    client: &kube::Client,
    namespace: &str,
    service_name: &str,
) -> std::result::Result<(), ReconcileError> {
    let endpoints: Api<Endpoints> = Api::namespaced(client.clone(), namespace);

    for attempt in 1..=60 {
        match endpoints.get(service_name).await {
            Ok(current)
                if current.subsets.as_ref().is_some_and(|subsets| {
                    subsets.iter().any(endpoint_subset_has_ready_target)
                }) =>
            {
                return Ok(());
            }
            Ok(_) => {
                info!(
                    "waiting for service '{namespace}/{service_name}' to publish a ready endpoint ({attempt}/60)"
                );
            }
            Err(err) => {
                info!(
                    "waiting for service '{namespace}/{service_name}' endpoint lookup ({attempt}/60): {err}"
                );
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }

    Err(ReconcileError::Store(format!(
        "service '{namespace}/{service_name}' did not publish a ready endpoint"
    )))
}

fn endpoint_subset_has_ready_target(subset: &k8s_openapi::api::core::v1::EndpointSubset) -> bool {
    subset
        .addresses
        .as_ref()
        .is_some_and(|addresses| !addresses.is_empty())
        && subset.ports.as_ref().is_some_and(|ports| !ports.is_empty())
}

fn deployment_spec_for_test(test: &TestSpec, labels: &BTreeMap<String, String>) -> DeploymentSpec {
    let mut deployment = test.deployment.clone();
    merge_labels(&mut deployment.selector.match_labels, labels);
    let template_metadata = deployment
        .template
        .metadata
        .get_or_insert_with(ObjectMeta::default);
    merge_labels(&mut template_metadata.labels, labels);
    deployment
}

fn service_spec_for_test(
    test: &TestSpec,
    labels: &BTreeMap<String, String>,
) -> std::result::Result<ServiceSpec, ReconcileError> {
    let mut service = test.service.clone();
    merge_labels(&mut service.selector, labels);
    if service.ports.as_ref().is_none_or(Vec::is_empty) {
        return Err(ReconcileError::Store(format!(
            "test '{}' service spec must define at least one port",
            test.name
        )));
    }
    Ok(service)
}

fn merge_labels(target: &mut Option<BTreeMap<String, String>>, labels: &BTreeMap<String, String>) {
    let target = target.get_or_insert_with(BTreeMap::new);
    for (key, value) in labels {
        target.insert(key.clone(), value.clone());
    }
}

pub(super) fn test_instance_name(bgd: &BlueGreenDeployment, logical_name: &str) -> String {
    let bgd_name = bgd.metadata.name.as_deref().unwrap_or("bgd");
    let bgd_uid = bgd.metadata.uid.as_deref().unwrap_or("");
    let logical = sanitize_dns_label(logical_name);
    let logical = if logical.is_empty() {
        "test".to_string()
    } else {
        logical
    };
    let digest = Sha256::digest(format!("{bgd_name}:{bgd_uid}:{logical_name}").as_bytes());
    let mut suffix = String::with_capacity(12);
    for byte in digest.iter() {
        let ch = match byte % 36 {
            value @ 0..=9 => (b'0' + value) as char,
            value => (b'a' + (value - 10)) as char,
        };
        suffix.push(ch);
        if suffix.len() == 12 {
            break;
        }
    }
    let prefix = "fluidbg-test-";
    let max_base_len = 63usize.saturating_sub(prefix.len() + suffix.len() + 1);
    let trimmed = logical.chars().take(max_base_len).collect::<String>();
    format!("{prefix}{trimmed}-{suffix}")
}

fn sanitize_dns_label(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut last_was_dash = false;
    for ch in value.chars() {
        let lowered = ch.to_ascii_lowercase();
        if lowered.is_ascii_alphanumeric() {
            out.push(lowered);
            last_was_dash = false;
        } else if !last_was_dash {
            out.push('-');
            last_was_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

fn test_labels(
    bgd: &BlueGreenDeployment,
    logical_name: &str,
    runtime_name: &str,
) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("app".to_string(), runtime_name.to_string()),
        ("fluidbg.io/test".to_string(), logical_name.to_string()),
        (
            "fluidbg.io/blue-green-ref".to_string(),
            bgd.metadata.name.as_deref().unwrap_or("").to_string(),
        ),
        (
            "fluidbg.io/blue-green-uid".to_string(),
            bgd.metadata.uid.as_deref().unwrap_or("").to_string(),
        ),
    ])
}

async fn ensure_test_name_available(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
    logical_name: &str,
    runtime_name: &str,
    expected_labels: &BTreeMap<String, String>,
) -> std::result::Result<(), ReconcileError> {
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), namespace);
    let services: Api<Service> = Api::namespaced(client.clone(), namespace);

    if let Some(deployment) = deployments.get_opt(runtime_name).await?
        && !test_resource_matches(&deployment.metadata.labels, expected_labels)
    {
        return Err(test_name_collision_error(
            bgd,
            "deployment",
            namespace,
            logical_name,
            runtime_name,
        ));
    }
    if let Some(service) = services.get_opt(runtime_name).await?
        && !test_resource_matches(&service.metadata.labels, expected_labels)
    {
        return Err(test_name_collision_error(
            bgd,
            "service",
            namespace,
            logical_name,
            runtime_name,
        ));
    }

    Ok(())
}

fn test_resource_matches(
    labels: &Option<BTreeMap<String, String>>,
    expected_labels: &BTreeMap<String, String>,
) -> bool {
    let Some(labels) = labels else {
        return false;
    };
    expected_labels.iter().all(|(key, expected)| {
        labels
            .get(key)
            .map(|actual| actual == expected)
            .unwrap_or(false)
    })
}

fn test_name_collision_error(
    bgd: &BlueGreenDeployment,
    kind: &str,
    namespace: &str,
    logical_name: &str,
    runtime_name: &str,
) -> ReconcileError {
    ReconcileError::Store(format!(
        "generated test {kind} name '{namespace}/{runtime_name}' for BGD '{}' logical test '{}' collides with an existing resource owned by another rollout",
        bgd.metadata.name.as_deref().unwrap_or(""),
        logical_name
    ))
}

pub(super) async fn ensure_inception_point_owned_resources(
    client: &kube::Client,
    default_namespace: &str,
    ip: &InceptionPoint,
) -> std::result::Result<(), ReconcileError> {
    for manifest in &ip.resources {
        apply_dynamic_manifest(client, default_namespace, manifest).await?;
    }
    Ok(())
}

pub(super) async fn read_operator_signing_key(
    client: &kube::Client,
    namespace: &str,
    secret_name: &str,
    secret_key: &str,
) -> std::result::Result<Vec<u8>, ReconcileError> {
    let secrets: Api<Secret> = Api::namespaced(client.clone(), namespace);
    let secret = secrets.get(secret_name).await?;
    let key = secret
        .data
        .as_ref()
        .and_then(|data| data.get(secret_key))
        .map(|value| value.0.clone())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            ReconcileError::Store(format!(
                "operator auth Secret '{namespace}/{secret_name}' is missing non-empty key '{secret_key}'"
            ))
        })?;
    Ok(key)
}

pub(super) async fn sign_inception_auth_token(
    client: &kube::Client,
    namespace: &str,
    auth: &AuthConfig,
    blue_green_ref: &str,
    inception_point: &str,
    plugin: &InceptionPlugin,
) -> std::result::Result<String, ReconcileError> {
    let signing_key = read_operator_signing_key(
        client,
        &auth.signing_secret_namespace,
        &auth.signing_secret_name,
        &auth.signing_secret_key,
    )
    .await?;
    let bgd_api: Api<BlueGreenDeployment> = Api::namespaced(client.clone(), namespace);
    let blue_green_uid = bgd_api
        .get(blue_green_ref)
        .await?
        .metadata
        .uid
        .ok_or_else(|| {
            ReconcileError::Store(format!(
                "BlueGreenDeployment '{namespace}/{blue_green_ref}' has no uid"
            ))
        })?;
    let claims = fluidbg_plugin_sdk::PluginAuthClaims::new_with_uid(
        namespace,
        blue_green_ref,
        &blue_green_uid,
        inception_point,
        plugin.metadata.name.as_deref().unwrap_or(""),
    );
    fluidbg_plugin_sdk::sign_plugin_auth_token(&claims, &signing_key)
        .map_err(|err| ReconcileError::Store(format!("failed to sign plugin auth token: {err}")))
}

pub(super) async fn sign_manager_sync_auth_token(
    client: &kube::Client,
    auth: &AuthConfig,
    plugin: &InceptionPlugin,
) -> std::result::Result<String, ReconcileError> {
    let signing_key = read_operator_signing_key(
        client,
        &auth.signing_secret_namespace,
        &auth.signing_secret_name,
        &auth.signing_secret_key,
    )
    .await?;
    let claims = fluidbg_plugin_sdk::PluginAuthClaims::new(
        "fluidbg-system",
        "__manager_sync__",
        "__manager_sync__",
        plugin.metadata.name.as_deref().unwrap_or(""),
    );
    fluidbg_plugin_sdk::sign_plugin_auth_token(&claims, &signing_key)
        .map_err(|err| ReconcileError::Store(format!("failed to sign manager sync token: {err}")))
}

pub(super) async fn validate_inception_auth_token(
    client: &kube::Client,
    auth: &AuthConfig,
    header_value: Option<&str>,
) -> std::result::Result<Option<fluidbg_plugin_sdk::PluginAuthClaims>, ReconcileError> {
    let Some(token) = header_value.and_then(fluidbg_plugin_sdk::bearer_token) else {
        return Ok(None);
    };
    let signing_key = read_operator_signing_key(
        client,
        &auth.signing_secret_namespace,
        &auth.signing_secret_name,
        &auth.signing_secret_key,
    )
    .await?;
    match fluidbg_plugin_sdk::verify_plugin_auth_token(token, &signing_key) {
        Ok(claims) => Ok(Some(claims)),
        Err(_) => Ok(None),
    }
}

async fn cleanup_inception_point_owned_resources(
    client: &kube::Client,
    default_namespace: &str,
    ip: &InceptionPoint,
) -> std::result::Result<(), ReconcileError> {
    for manifest in &ip.resources {
        delete_dynamic_manifest(client, default_namespace, manifest).await?;
    }
    Ok(())
}

async fn apply_dynamic_manifest(
    client: &kube::Client,
    default_namespace: &str,
    manifest: &Value,
) -> std::result::Result<(), ReconcileError> {
    let identity = manifest_identity(default_namespace, manifest)?;
    let mut object: DynamicObject = serde_json::from_value(manifest.clone()).map_err(|err| {
        ReconcileError::Store(format!(
            "failed to deserialize dynamic resource {}/{}/{}: {err}",
            identity.api_version, identity.kind, identity.name
        ))
    })?;
    object
        .metadata
        .namespace
        .get_or_insert_with(|| identity.namespace.clone());
    let api: Api<DynamicObject> =
        Api::namespaced_with(client.clone(), &identity.namespace, &identity.api_resource);
    let pp = PatchParams::apply("fluidbg-operator").force();
    api.patch(&identity.name, &pp, &Patch::Apply(&object))
        .await?;
    Ok(())
}

async fn delete_dynamic_manifest(
    client: &kube::Client,
    default_namespace: &str,
    manifest: &Value,
) -> std::result::Result<(), ReconcileError> {
    let identity = manifest_identity(default_namespace, manifest)?;
    let api: Api<DynamicObject> =
        Api::namespaced_with(client.clone(), &identity.namespace, &identity.api_resource);
    delete_resource(api, &identity.kind, &identity.namespace, &identity.name).await
}

struct DynamicManifestIdentity {
    api_version: String,
    kind: String,
    name: String,
    namespace: String,
    api_resource: ApiResource,
}

fn manifest_identity(
    default_namespace: &str,
    manifest: &Value,
) -> std::result::Result<DynamicManifestIdentity, ReconcileError> {
    let api_version = manifest
        .get("apiVersion")
        .and_then(Value::as_str)
        .ok_or_else(|| ReconcileError::Store("resource manifest is missing apiVersion".into()))?;
    let kind = manifest
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| ReconcileError::Store("resource manifest is missing kind".into()))?;
    let name = manifest
        .get("metadata")
        .and_then(|value| value.get("name"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ReconcileError::Store("resource manifest is missing metadata.name".into())
        })?;
    let namespace = manifest
        .get("metadata")
        .and_then(|value| value.get("namespace"))
        .and_then(Value::as_str)
        .unwrap_or(default_namespace);

    let (group, version) = match api_version.split_once('/') {
        Some((group, version)) => (group, version),
        None => ("", api_version),
    };
    let gvk = GroupVersionKind::gvk(group, version, kind);
    Ok(DynamicManifestIdentity {
        api_version: api_version.to_string(),
        kind: kind.to_string(),
        name: name.to_string(),
        namespace: namespace.to_string(),
        api_resource: ApiResource::from_gvk(&gvk),
    })
}

pub(super) async fn apply_deployment_manifest(
    client: &kube::Client,
    bgd: &BlueGreenDeployment,
    default_namespace: &str,
    deployment_spec: &ManagedDeploymentSpec,
    family_labels: &BTreeMap<String, String>,
    is_green: bool,
    initial_assignments: &[PropertyAssignment],
) -> std::result::Result<DeploymentIdentity, ReconcileError> {
    let candidate = candidate_ref(bgd);
    let mut spec = if is_green {
        deployment_spec.spec.clone()
    } else {
        deployment_spec_with_test_patch(bgd, &deployment_spec.spec)?
    };
    if !is_green {
        apply_assignments_to_deployment_spec(
            &mut spec,
            initial_assignments,
            AssignmentTarget::Blue,
        )?;
    }
    let mut deployment = Deployment {
        metadata: ObjectMeta {
            name: Some(candidate.name.clone()),
            namespace: Some(deployment_namespace_spec(
                deployment_spec,
                default_namespace,
            )),
            ..Default::default()
        },
        spec: Some(spec),
        ..Default::default()
    };
    apply_family_labels_to_deployment(&mut deployment, family_labels)?;
    set_green_label(&mut deployment, is_green)?;
    if is_green {
        clear_rollout_candidate_labels(&mut deployment)?;
    } else {
        apply_rollout_candidate_labels(&mut deployment, bgd)?;
    }

    let name = deployment
        .metadata
        .name
        .clone()
        .unwrap_or_else(|| candidate.name.clone());
    let deploy_namespace = deployment
        .metadata
        .namespace
        .clone()
        .unwrap_or_else(|| deployment_namespace_spec(deployment_spec, default_namespace));
    apply_runtime_identity_selector(&mut deployment, &name)?;

    let deploy_api: Api<Deployment> = Api::namespaced(client.clone(), &deploy_namespace);
    match deploy_api.get(&name).await {
        Ok(existing) => {
            deployment.metadata.resource_version = existing.metadata.resource_version;
            deploy_api
                .replace(&name, &Default::default(), &deployment)
                .await?;
        }
        Err(kube::Error::Api(err)) if err.code == 404 => {
            deploy_api
                .create(&PostParams::default(), &deployment)
                .await?;
        }
        Err(err) => return Err(ReconcileError::K8s(err)),
    }
    info!(
        "applied declared deployment '{}/{}'",
        deploy_namespace, name
    );
    Ok(DeploymentIdentity {
        namespace: deploy_namespace,
        name,
    })
}

pub(super) fn deployment_spec_with_test_patch(
    bgd: &BlueGreenDeployment,
    base: &DeploymentSpec,
) -> std::result::Result<DeploymentSpec, ReconcileError> {
    let Some(patch) = bgd.spec.test_deployment_patch.as_ref() else {
        return Ok(base.clone());
    };
    let mut value = serde_json::to_value(base).map_err(|err| {
        ReconcileError::Store(format!(
            "failed to encode deployment spec for patching: {err}"
        ))
    })?;
    let patch = serde_json::to_value(patch).map_err(|err| {
        ReconcileError::Store(format!(
            "failed to encode spec.testDeploymentPatch for patching: {err}"
        ))
    })?;
    merge_json_value(&mut value, &patch);
    serde_json::from_value(value).map_err(|err| {
        ReconcileError::Store(format!(
            "spec.testDeploymentPatch does not produce a valid apps/v1 DeploymentSpec: {err}"
        ))
    })
}

fn merge_json_value(base: &mut Value, patch: &Value) {
    match (base, patch) {
        (Value::Object(base), Value::Object(patch)) => {
            for (key, patch_value) in patch {
                if patch_value.is_null() {
                    base.remove(key);
                } else {
                    merge_json_value(base.entry(key.clone()).or_insert(Value::Null), patch_value);
                }
            }
        }
        (base, patch) => {
            *base = patch.clone();
        }
    }
}

fn apply_runtime_identity_selector(
    deployment: &mut Deployment,
    name: &str,
) -> std::result::Result<(), ReconcileError> {
    let spec = deployment
        .spec
        .as_mut()
        .ok_or_else(|| ReconcileError::Store("deployment spec missing".to_string()))?;
    spec.selector
        .match_labels
        .get_or_insert_with(Default::default)
        .insert("fluidbg.io/deployment".to_string(), name.to_string());

    let template_labels = spec
        .template
        .metadata
        .as_mut()
        .map(|metadata| metadata.labels.get_or_insert_with(Default::default))
        .ok_or_else(|| ReconcileError::Store("deployment template metadata missing".to_string()))?;
    template_labels.insert("fluidbg.io/deployment".to_string(), name.to_string());
    Ok(())
}

pub(super) async fn apply_resource<K>(
    api: Api<K>,
    name: &str,
    resource: &K,
) -> std::result::Result<(), ReconcileError>
where
    K: kube::Resource<DynamicType = ()>
        + Clone
        + serde::de::DeserializeOwned
        + serde::Serialize
        + std::fmt::Debug,
{
    let pp = PatchParams::apply("fluidbg-operator").force();
    api.patch(name, &pp, &Patch::Apply(resource)).await?;
    Ok(())
}

pub(super) async fn cleanup_test_resources(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
) -> std::result::Result<(), ReconcileError> {
    for test in &bgd.spec.tests {
        let test_name = test_instance_name(bgd, &test.name);
        delete_deployment(client, namespace, &test_name).await?;
        delete_service(client, namespace, &test_name).await?;
    }

    wait_for_test_resources_deleted(bgd, client, namespace).await
}

pub(super) async fn cleanup_inception_resources(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
) -> std::result::Result<(), ReconcileError> {
    let blue_green_ref = bgd.metadata.name.as_deref().unwrap_or("");
    for ip in &bgd.spec.inception_points {
        cleanup_inception_point_owned_resources(client, namespace, ip).await?;
        let deployment_name = inception_instance_base_name(blue_green_ref, &ip.name);
        let service_name = inception_service_name(blue_green_ref, &ip.name);
        let config_map_name = inception_config_map_name(blue_green_ref, &ip.name);
        let auth_secret_name = inception_auth_secret_name(blue_green_ref, &ip.name);
        delete_deployment(client, namespace, &deployment_name).await?;
        delete_service(client, namespace, &service_name).await?;
        delete_config_map(client, namespace, &config_map_name).await?;
        delete_secret(client, namespace, &auth_secret_name).await?;
    }
    wait_for_inception_resources_deleted(client, namespace, blue_green_ref).await
}

pub(super) async fn cleanup_orphaned_blue_green_resources(
    client: &kube::Client,
    namespace: &str,
    blue_green_ref: &str,
) -> std::result::Result<(), ReconcileError> {
    let selector = blue_green_ref_selector(blue_green_ref);
    delete_labeled_resources::<Deployment>(client, namespace, "deployment", &selector).await?;
    delete_labeled_resources::<Service>(client, namespace, "service", &selector).await?;
    delete_labeled_resources::<ConfigMap>(client, namespace, "configmap", &selector).await?;
    delete_labeled_resources::<Secret>(client, namespace, "secret", &selector).await?;
    delete_labeled_resources::<Pod>(client, namespace, "pod", &selector).await?;
    wait_for_blue_green_resources_deleted(client, namespace, blue_green_ref).await
}

pub(super) async fn blue_green_refs_from_owned_resources(
    client: &kube::Client,
    namespace: &str,
) -> std::result::Result<BTreeSet<String>, ReconcileError> {
    let mut refs = BTreeSet::new();
    collect_blue_green_refs::<Deployment>(client, namespace, &mut refs).await?;
    collect_blue_green_refs::<Service>(client, namespace, &mut refs).await?;
    collect_blue_green_refs::<ConfigMap>(client, namespace, &mut refs).await?;
    collect_blue_green_refs::<Secret>(client, namespace, &mut refs).await?;
    collect_blue_green_refs::<Pod>(client, namespace, &mut refs).await?;
    Ok(refs)
}

pub(super) async fn blue_green_refs_from_owned_resources_all(
    client: &kube::Client,
) -> std::result::Result<BTreeSet<String>, ReconcileError> {
    let mut refs = BTreeSet::new();
    let namespaces: Api<Namespace> = Api::all(client.clone());
    for namespace in namespaces.list(&Default::default()).await?.items {
        let Some(namespace) = namespace.metadata.name else {
            continue;
        };
        refs.extend(
            blue_green_refs_from_owned_resources(client, &namespace)
                .await?
                .into_iter()
                .map(|name| blue_green_state_key(&namespace, &name)),
        );
    }
    Ok(refs)
}

pub(super) async fn delete_deployment(
    client: &kube::Client,
    namespace: &str,
    name: &str,
) -> std::result::Result<(), ReconcileError> {
    let api: Api<Deployment> = Api::namespaced(client.clone(), namespace);
    delete_resource(api, "deployment", namespace, name).await
}

async fn delete_service(
    client: &kube::Client,
    namespace: &str,
    name: &str,
) -> std::result::Result<(), ReconcileError> {
    let api: Api<Service> = Api::namespaced(client.clone(), namespace);
    delete_resource(api, "service", namespace, name).await
}

async fn delete_config_map(
    client: &kube::Client,
    namespace: &str,
    name: &str,
) -> std::result::Result<(), ReconcileError> {
    let api: Api<ConfigMap> = Api::namespaced(client.clone(), namespace);
    delete_resource(api, "configmap", namespace, name).await
}

async fn delete_secret(
    client: &kube::Client,
    namespace: &str,
    name: &str,
) -> std::result::Result<(), ReconcileError> {
    let api: Api<Secret> = Api::namespaced(client.clone(), namespace);
    delete_resource(api, "secret", namespace, name).await
}

async fn collect_blue_green_refs<K>(
    client: &kube::Client,
    namespace: &str,
    refs: &mut BTreeSet<String>,
) -> std::result::Result<(), ReconcileError>
where
    K: kube::Resource<DynamicType = (), Scope = NamespaceResourceScope>
        + Clone
        + serde::de::DeserializeOwned
        + std::fmt::Debug,
{
    let api: Api<K> = Api::namespaced(client.clone(), namespace);
    let params = ListParams::default().labels("fluidbg.io/blue-green-ref");
    for item in api.list(&params).await?.items {
        if let Some(value) = item.labels().get("fluidbg.io/blue-green-ref") {
            refs.insert(value.clone());
        }
    }
    Ok(())
}

async fn delete_labeled_resources<K>(
    client: &kube::Client,
    namespace: &str,
    kind: &str,
    selector: &str,
) -> std::result::Result<(), ReconcileError>
where
    K: kube::Resource<DynamicType = (), Scope = NamespaceResourceScope>
        + Clone
        + serde::de::DeserializeOwned
        + std::fmt::Debug
        + Send
        + Sync
        + 'static,
{
    let api: Api<K> = Api::namespaced(client.clone(), namespace);
    let params = ListParams::default().labels(selector);
    for item in api.list(&params).await?.items {
        delete_resource(api.clone(), kind, namespace, &item.name_any()).await?;
    }
    Ok(())
}

fn blue_green_ref_selector(blue_green_ref: &str) -> String {
    format!("fluidbg.io/blue-green-ref={blue_green_ref}")
}

async fn wait_for_inception_resources_deleted(
    client: &kube::Client,
    namespace: &str,
    blue_green_ref: &str,
) -> std::result::Result<(), ReconcileError> {
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), namespace);
    let services: Api<Service> = Api::namespaced(client.clone(), namespace);
    let config_maps: Api<ConfigMap> = Api::namespaced(client.clone(), namespace);
    let secrets: Api<Secret> = Api::namespaced(client.clone(), namespace);
    let pods: Api<Pod> = Api::namespaced(client.clone(), namespace);
    let params = ListParams::default().labels(&format!(
        "fluidbg.io/blue-green-ref={blue_green_ref},fluidbg.io/inception-point"
    ));

    for attempt in 1..=60 {
        let deployment_count = deployments.list(&params).await?.items.len();
        let service_count = services.list(&params).await?.items.len();
        let config_map_count = config_maps.list(&params).await?.items.len();
        let secret_count = secrets.list(&params).await?.items.len();
        let pod_count = pods.list(&params).await?.items.len();
        if deployment_count == 0
            && service_count == 0
            && config_map_count == 0
            && secret_count == 0
            && pod_count == 0
        {
            return Ok(());
        }
        info!(
            "waiting for inception resources for BGD '{}' to disappear: deployments={} services={} configmaps={} secrets={} pods={} ({}/60)",
            blue_green_ref,
            deployment_count,
            service_count,
            config_map_count,
            secret_count,
            pod_count,
            attempt
        );
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }

    Err(ReconcileError::Store(format!(
        "inception resources for BGD '{blue_green_ref}' were not deleted in namespace {namespace}"
    )))
}

async fn wait_for_blue_green_resources_deleted(
    client: &kube::Client,
    namespace: &str,
    blue_green_ref: &str,
) -> std::result::Result<(), ReconcileError> {
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), namespace);
    let services: Api<Service> = Api::namespaced(client.clone(), namespace);
    let config_maps: Api<ConfigMap> = Api::namespaced(client.clone(), namespace);
    let secrets: Api<Secret> = Api::namespaced(client.clone(), namespace);
    let pods: Api<Pod> = Api::namespaced(client.clone(), namespace);
    let selector = blue_green_ref_selector(blue_green_ref);
    let params = ListParams::default().labels(&selector);

    for attempt in 1..=60 {
        let deployment_count = deployments.list(&params).await?.items.len();
        let service_count = services.list(&params).await?.items.len();
        let config_map_count = config_maps.list(&params).await?.items.len();
        let secret_count = secrets.list(&params).await?.items.len();
        let pod_count = pods.list(&params).await?.items.len();
        if deployment_count == 0
            && service_count == 0
            && config_map_count == 0
            && secret_count == 0
            && pod_count == 0
        {
            return Ok(());
        }
        info!(
            "waiting for orphaned resources for BGD '{}' to disappear: deployments={} services={} configmaps={} secrets={} pods={} ({}/60)",
            blue_green_ref,
            deployment_count,
            service_count,
            config_map_count,
            secret_count,
            pod_count,
            attempt
        );
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }

    Err(ReconcileError::Store(format!(
        "orphaned resources for BGD '{blue_green_ref}' were not deleted in namespace {namespace}"
    )))
}

async fn wait_for_test_resources_deleted(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
) -> std::result::Result<(), ReconcileError> {
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), namespace);
    let services: Api<Service> = Api::namespaced(client.clone(), namespace);

    for test in &bgd.spec.tests {
        let test_name = test_instance_name(bgd, &test.name);
        for attempt in 1..=60 {
            let deployment_exists = deployments.get_opt(&test_name).await?.is_some();
            let service_exists = services.get_opt(&test_name).await?.is_some();
            if !deployment_exists && !service_exists {
                break;
            }
            info!(
                "waiting for test resources for BGD '{}' logical test '{}' to disappear: deployment={} service={} ({}/60)",
                bgd.metadata.name.as_deref().unwrap_or(""),
                test.name,
                deployment_exists,
                service_exists,
                attempt
            );
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }

        if deployments.get_opt(&test_name).await?.is_some()
            || services.get_opt(&test_name).await?.is_some()
        {
            return Err(ReconcileError::Store(format!(
                "test resources for BGD '{}' logical test '{}' were not deleted in namespace {namespace}",
                bgd.metadata.name.as_deref().unwrap_or(""),
                test.name
            )));
        }
    }

    Ok(())
}

async fn delete_resource<K>(
    api: Api<K>,
    kind: &str,
    namespace: &str,
    name: &str,
) -> std::result::Result<(), ReconcileError>
where
    K: kube::Resource
        + Clone
        + serde::de::DeserializeOwned
        + std::fmt::Debug
        + Send
        + Sync
        + 'static,
{
    match api.delete(name, &DeleteParams::default()).await {
        Ok(_) => {
            info!("deleted test {} '{}/{}'", kind, namespace, name);
            Ok(())
        }
        Err(kube::Error::Api(e)) if e.code == 404 => Ok(()),
        Err(e) => Err(ReconcileError::K8s(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        apply_assignments_to_deployment_spec, deployment_spec_for_test,
        deployment_spec_with_test_patch, service_spec_for_test, test_instance_name,
        test_service_port,
    };
    use crate::controller::plugin_lifecycle::{
        AssignmentKind, AssignmentTarget, PropertyAssignment,
    };
    use crate::crd::blue_green::{
        BlueGreenDeployment, BlueGreenDeploymentSpec, DeploymentSelector, ManagedDeploymentSpec,
        TestDeploymentPatch, TestSpec,
    };
    use k8s_openapi::api::apps::v1::DeploymentSpec;
    use k8s_openapi::api::core::v1::{
        Container, EnvVar, HTTPGetAction, PodSpec, PodTemplateSpec, Probe, ServicePort, ServiceSpec,
    };
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta};
    use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
    use std::collections::BTreeMap;

    fn bgd(name: &str) -> BlueGreenDeployment {
        bgd_with_uid(name, &format!("{name}-uid"))
    }

    fn bgd_with_uid(name: &str, uid: &str) -> BlueGreenDeployment {
        BlueGreenDeployment {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
                uid: Some(uid.to_string()),
                ..Default::default()
            },
            spec: BlueGreenDeploymentSpec {
                selector: DeploymentSelector {
                    namespace: None,
                    match_labels: BTreeMap::new(),
                },
                deployment: ManagedDeploymentSpec {
                    namespace: None,
                    spec: Default::default(),
                },
                test_deployment_patch: None,
                inception_points: Vec::new(),
                tests: Vec::new(),
                promotion: None,
            },
            status: None,
        }
    }

    #[test]
    fn test_instance_name_is_stable_for_same_bgd() {
        assert_eq!(
            test_instance_name(&bgd("rollout-a"), "test-container"),
            test_instance_name(&bgd("rollout-a"), "test-container")
        );
    }

    #[test]
    fn test_instance_name_is_scoped_by_bgd_name() {
        assert_ne!(
            test_instance_name(&bgd("rollout-a"), "test-container"),
            test_instance_name(&bgd("rollout-b"), "test-container")
        );
    }

    #[test]
    fn test_instance_name_is_scoped_by_bgd_uid() {
        assert_ne!(
            test_instance_name(&bgd_with_uid("rollout-a", "uid-a"), "test-container"),
            test_instance_name(&bgd_with_uid("rollout-a", "uid-b"), "test-container")
        );
    }

    #[test]
    fn test_instance_name_keeps_logical_name_prefix() {
        assert!(
            test_instance_name(&bgd("rollout-a"), "test-container")
                .starts_with("fluidbg-test-test-container-")
        );
    }

    #[test]
    fn test_instance_name_is_service_safe() {
        let name = test_instance_name(&bgd("rollout-a"), &"x".repeat(200));
        assert!(name.len() <= 63);
        assert!(
            name.chars()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
        );
    }

    #[test]
    fn native_test_deployment_keeps_kubernetes_fields_and_adds_fluidbg_labels() {
        let labels = BTreeMap::from([("fluidbg.io/test".to_string(), "verifier".to_string())]);
        let spec = deployment_spec_for_test(
            &TestSpec {
                name: "verifier".to_string(),
                deployment: DeploymentSpec {
                    selector: LabelSelector {
                        match_labels: Some(BTreeMap::from([(
                            "app".to_string(),
                            "verifier".to_string(),
                        )])),
                        ..Default::default()
                    },
                    template: PodTemplateSpec {
                        metadata: Some(ObjectMeta {
                            labels: Some(BTreeMap::from([(
                                "app".to_string(),
                                "verifier".to_string(),
                            )])),
                            ..Default::default()
                        }),
                        spec: Some(PodSpec {
                            containers: vec![Container {
                                name: "verifier".to_string(),
                                image: Some("verifier:dev".to_string()),
                                env: Some(vec![EnvVar {
                                    name: "AMQP_URL".to_string(),
                                    value: Some("amqp://rabbitmq".to_string()),
                                    ..Default::default()
                                }]),
                                readiness_probe: Some(Probe {
                                    http_get: Some(HTTPGetAction {
                                        path: Some("/health".to_string()),
                                        port: IntOrString::Int(8080),
                                        ..Default::default()
                                    }),
                                    ..Default::default()
                                }),
                                ..Default::default()
                            }],
                            ..Default::default()
                        }),
                    },
                    ..Default::default()
                },
                service: ServiceSpec {
                    ports: Some(vec![ServicePort {
                        port: 8080,
                        ..Default::default()
                    }]),
                    ..Default::default()
                },
                data_verification: None,
                custom_verification: None,
            },
            &labels,
        );

        assert_eq!(
            spec.selector
                .match_labels
                .as_ref()
                .unwrap()
                .get("fluidbg.io/test"),
            Some(&"verifier".to_string())
        );
        assert_eq!(
            spec.template
                .metadata
                .as_ref()
                .unwrap()
                .labels
                .as_ref()
                .unwrap()
                .get("fluidbg.io/test"),
            Some(&"verifier".to_string())
        );
        let pod_spec = spec.template.spec.unwrap();
        let probe = pod_spec.containers[0]
            .readiness_probe
            .as_ref()
            .unwrap()
            .http_get
            .as_ref()
            .unwrap();
        assert_eq!(probe.path.as_deref(), Some("/health"));
        assert_eq!(
            pod_spec.containers[0].env.as_ref().unwrap()[0].name,
            "AMQP_URL"
        );
    }

    #[test]
    fn test_env_assignments_are_applied_before_test_deployment_is_created() {
        let mut spec = DeploymentSpec {
            selector: LabelSelector {
                match_labels: Some(BTreeMap::from([(
                    "app".to_string(),
                    "verifier".to_string(),
                )])),
                ..Default::default()
            },
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(BTreeMap::from([(
                        "app".to_string(),
                        "verifier".to_string(),
                    )])),
                    ..Default::default()
                }),
                spec: Some(PodSpec {
                    containers: vec![Container {
                        name: "verifier".to_string(),
                        image: Some("verifier:dev".to_string()),
                        env: Some(vec![EnvVar {
                            name: "EXISTING".to_string(),
                            value: Some("kept".to_string()),
                            ..Default::default()
                        }]),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
            },
            ..Default::default()
        };

        apply_assignments_to_deployment_spec(
            &mut spec,
            &[
                PropertyAssignment {
                    target: AssignmentTarget::Test,
                    kind: AssignmentKind::Env,
                    name: "HTTP_WRITE_URL".to_string(),
                    value: "http://plugin:9090".to_string(),
                    container_name: None,
                },
                PropertyAssignment {
                    target: AssignmentTarget::Blue,
                    kind: AssignmentKind::Env,
                    name: "INPUT_QUEUE".to_string(),
                    value: "ignored-for-tests".to_string(),
                    container_name: None,
                },
            ],
            AssignmentTarget::Test,
        )
        .unwrap();

        let env = spec.template.spec.as_ref().unwrap().containers[0]
            .env
            .as_ref()
            .unwrap();
        assert!(env.iter().any(|var| var.name == "EXISTING"));
        assert!(env.iter().any(|var| var.name == "HTTP_WRITE_URL"
            && var.value.as_deref() == Some("http://plugin:9090")));
        assert!(!env.iter().any(|var| var.name == "INPUT_QUEUE"));
    }

    #[test]
    fn blue_assignments_are_applied_to_candidate_spec_before_creation() {
        let mut spec = DeploymentSpec {
            template: PodTemplateSpec {
                spec: Some(PodSpec {
                    containers: vec![Container {
                        name: "app".to_string(),
                        env: Some(vec![EnvVar {
                            name: "EXISTING".to_string(),
                            value: Some("keep".to_string()),
                            ..Default::default()
                        }]),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };

        apply_assignments_to_deployment_spec(
            &mut spec,
            &[PropertyAssignment {
                target: AssignmentTarget::Blue,
                kind: AssignmentKind::Env,
                name: "INPUT_QUEUE".to_string(),
                value: "orders-blue-temp".to_string(),
                container_name: None,
            }],
            AssignmentTarget::Blue,
        )
        .unwrap();

        let env = spec.template.spec.as_ref().unwrap().containers[0]
            .env
            .as_ref()
            .unwrap();
        assert!(env.iter().any(|var| var.name == "EXISTING"));
        assert!(env.iter().any(
            |var| var.name == "INPUT_QUEUE" && var.value.as_deref() == Some("orders-blue-temp")
        ));
    }

    #[test]
    fn native_test_service_keeps_kubernetes_port_and_adds_selector_labels() {
        let labels = BTreeMap::from([("fluidbg.io/test".to_string(), "verifier".to_string())]);
        let test = TestSpec {
            name: "verifier".to_string(),
            deployment: DeploymentSpec::default(),
            service: ServiceSpec {
                selector: Some(BTreeMap::from([(
                    "app".to_string(),
                    "verifier".to_string(),
                )])),
                ports: Some(vec![ServicePort {
                    name: Some("http".to_string()),
                    port: 9090,
                    target_port: Some(IntOrString::String("http".to_string())),
                    ..Default::default()
                }]),
                ..Default::default()
            },
            data_verification: None,
            custom_verification: None,
        };

        let spec = service_spec_for_test(&test, &labels).unwrap();

        assert_eq!(test_service_port(&test).unwrap(), 9090);
        assert_eq!(
            spec.selector.as_ref().unwrap().get("fluidbg.io/test"),
            Some(&"verifier".to_string())
        );
        assert_eq!(
            spec.ports.as_ref().unwrap()[0].target_port,
            Some(IntOrString::String("http".to_string()))
        );
    }

    #[test]
    fn test_deployment_patch_overlays_optional_fields_only() {
        let mut bgd = bgd("rollout-a");
        bgd.spec.test_deployment_patch = Some(TestDeploymentPatch {
            replicas: Some(2),
            template: Some(PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    annotations: Some(BTreeMap::from([(
                        "fluidbg.io/test-mode".to_string(),
                        "true".to_string(),
                    )])),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        });
        let base = DeploymentSpec {
            replicas: Some(10),
            selector: LabelSelector {
                match_labels: Some(BTreeMap::from([("app".to_string(), "orders".to_string())])),
                ..Default::default()
            },
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(BTreeMap::from([("app".to_string(), "orders".to_string())])),
                    ..Default::default()
                }),
                spec: Some(PodSpec {
                    containers: vec![Container {
                        name: "orders".to_string(),
                        image: Some("orders:prod".to_string()),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
            },
            ..Default::default()
        };

        let patched = deployment_spec_with_test_patch(&bgd, &base).unwrap();

        assert_eq!(patched.replicas, Some(2));
        assert_eq!(
            patched.selector.match_labels.as_ref().unwrap().get("app"),
            Some(&"orders".to_string())
        );
        assert_eq!(
            patched
                .template
                .metadata
                .as_ref()
                .unwrap()
                .labels
                .as_ref()
                .unwrap()
                .get("app"),
            Some(&"orders".to_string())
        );
        assert_eq!(
            patched
                .template
                .metadata
                .as_ref()
                .unwrap()
                .annotations
                .as_ref()
                .unwrap()
                .get("fluidbg.io/test-mode"),
            Some(&"true".to_string())
        );
        assert_eq!(
            patched.template.spec.as_ref().unwrap().containers[0]
                .image
                .as_deref(),
            Some("orders:prod")
        );
    }
}
