use std::collections::BTreeMap;

use k8s_openapi::api::apps::v1::{Deployment, DeploymentSpec};
use k8s_openapi::api::core::v1::{
    ConfigMap, Container, ContainerPort, EnvVar, Pod, PodSpec, PodTemplateSpec, Secret, Service,
    ServicePort, ServiceSpec,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::api::{Api, DeleteParams, ListParams, Patch, PatchParams, PostParams};
use kube::api::{ApiResource, DynamicObject, GroupVersionKind};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tracing::info;

use crate::crd::blue_green::{BlueGreenDeployment, InceptionPoint, ManagedDeploymentSpec};
use crate::crd::inception_plugin::InceptionPlugin;
use crate::plugins::reconciler::{
    inception_auth_secret_name, inception_config_map_name, inception_instance_base_name,
    inception_service_name,
};

use super::{
    AuthConfig, ReconcileError, apply_family_labels_to_deployment, candidate_ref,
    deployment_namespace_spec, set_green_label,
};

pub(super) async fn ensure_test_resources(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
) -> std::result::Result<(), ReconcileError> {
    for test in &bgd.spec.tests {
        let test_name = test_instance_name(bgd, &test.name);
        let labels = test_labels(bgd, &test.name, &test_name);
        ensure_test_name_available(bgd, client, namespace, &test.name, &test_name, &labels).await?;
        let env = test
            .env
            .iter()
            .map(|env| EnvVar {
                name: env.name.clone(),
                value: Some(env.value.clone()),
                ..Default::default()
            })
            .collect::<Vec<_>>();

        let deployment = Deployment {
            metadata: ObjectMeta {
                name: Some(test_name.clone()),
                namespace: Some(namespace.to_string()),
                labels: Some(labels.clone()),
                ..Default::default()
            },
            spec: Some(DeploymentSpec {
                replicas: Some(1),
                selector: LabelSelector {
                    match_labels: Some(labels.clone()),
                    ..Default::default()
                },
                template: PodTemplateSpec {
                    metadata: Some(ObjectMeta {
                        labels: Some(labels.clone()),
                        ..Default::default()
                    }),
                    spec: Some(PodSpec {
                        containers: vec![Container {
                            name: test_name.clone(),
                            image: Some(test.image.clone()),
                            image_pull_policy: Some("Never".to_string()),
                            ports: Some(vec![ContainerPort {
                                container_port: test.port,
                                ..Default::default()
                            }]),
                            env: Some(env),
                            ..Default::default()
                        }],
                        termination_grace_period_seconds: Some(1),
                        ..Default::default()
                    }),
                },
                ..Default::default()
            }),
            ..Default::default()
        };
        apply_resource(
            Api::namespaced(client.clone(), namespace),
            &test_name,
            &deployment,
        )
        .await?;

        let service = Service {
            metadata: ObjectMeta {
                name: Some(test_name.clone()),
                namespace: Some(namespace.to_string()),
                labels: Some(labels.clone()),
                ..Default::default()
            },
            spec: Some(ServiceSpec {
                selector: Some(labels),
                ports: Some(vec![ServicePort {
                    port: test.port,
                    target_port: Some(IntOrString::Int(test.port)),
                    ..Default::default()
                }]),
                ..Default::default()
            }),
            ..Default::default()
        };
        apply_resource(
            Api::namespaced(client.clone(), namespace),
            &test_name,
            &service,
        )
        .await?;
    }

    Ok(())
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
    let claims = fluidbg_plugin_sdk::PluginAuthClaims::new(
        namespace,
        blue_green_ref,
        inception_point,
        plugin.metadata.name.as_deref().unwrap_or(""),
    );
    fluidbg_plugin_sdk::sign_plugin_auth_token(&claims, &signing_key)
        .map_err(|err| ReconcileError::Store(format!("failed to sign plugin auth token: {err}")))
}

pub(super) async fn validate_inception_auth_token(
    client: &kube::Client,
    namespace: &str,
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
        Ok(claims) if claims.namespace == namespace => Ok(Some(claims)),
        Ok(_) => Ok(None),
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
) -> std::result::Result<(), ReconcileError> {
    let candidate = candidate_ref(bgd);
    let mut deployment = Deployment {
        metadata: ObjectMeta {
            name: Some(candidate.name.clone()),
            namespace: Some(deployment_namespace_spec(
                deployment_spec,
                default_namespace,
            )),
            ..Default::default()
        },
        spec: Some(deployment_spec.spec.clone()),
        ..Default::default()
    };
    apply_family_labels_to_deployment(&mut deployment, family_labels)?;
    set_green_label(&mut deployment, is_green)?;

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
    Ok(())
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
    use super::test_instance_name;
    use crate::crd::blue_green::{
        BlueGreenDeployment, BlueGreenDeploymentSpec, DeploymentSelector, ManagedDeploymentSpec,
    };
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
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
}
