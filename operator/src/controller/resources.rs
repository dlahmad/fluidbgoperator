use std::collections::BTreeMap;

use k8s_openapi::api::apps::v1::{Deployment, DeploymentSpec};
use k8s_openapi::api::core::v1::{
    ConfigMap, Container, ContainerPort, EnvVar, Pod, PodSpec, PodTemplateSpec, Service,
    ServicePort, ServiceSpec,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::api::{Api, DeleteParams, ListParams, Patch, PatchParams, PostParams};
use kube::api::{ApiResource, DynamicObject, GroupVersionKind};
use serde_json::Value;
use tracing::info;

use crate::crd::blue_green::{BlueGreenDeployment, InceptionPoint, ManagedDeploymentSpec};
use crate::plugins::reconciler::{
    inception_config_map_name, inception_instance_base_name, inception_service_name,
};

use super::{
    ReconcileError, apply_family_labels_to_deployment, candidate_ref, deployment_namespace_spec,
    set_green_label,
};

pub(super) async fn ensure_test_resources(
    bgd: &BlueGreenDeployment,
    client: &kube::Client,
    namespace: &str,
) -> std::result::Result<(), ReconcileError> {
    for test in &bgd.spec.tests {
        let env = test
            .env
            .iter()
            .map(|env| EnvVar {
                name: env.name.clone(),
                value: Some(env.value.clone()),
                ..Default::default()
            })
            .collect::<Vec<_>>();

        let labels = BTreeMap::from([
            ("app".to_string(), test.name.clone()),
            ("fluidbg.io/test".to_string(), test.name.clone()),
        ]);
        let deployment = Deployment {
            metadata: ObjectMeta {
                name: Some(test.name.clone()),
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
                            name: test.name.clone(),
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
            &test.name,
            &deployment,
        )
        .await?;

        let service = Service {
            metadata: ObjectMeta {
                name: Some(test.name.clone()),
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
            &test.name,
            &service,
        )
        .await?;
    }

    Ok(())
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
        delete_deployment(client, namespace, &test.name).await?;
        delete_service(client, namespace, &test.name).await?;
    }

    Ok(())
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
        delete_deployment(client, namespace, &deployment_name).await?;
        delete_service(client, namespace, &service_name).await?;
        delete_config_map(client, namespace, &config_map_name).await?;
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

async fn wait_for_inception_resources_deleted(
    client: &kube::Client,
    namespace: &str,
    blue_green_ref: &str,
) -> std::result::Result<(), ReconcileError> {
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), namespace);
    let services: Api<Service> = Api::namespaced(client.clone(), namespace);
    let config_maps: Api<ConfigMap> = Api::namespaced(client.clone(), namespace);
    let pods: Api<Pod> = Api::namespaced(client.clone(), namespace);
    let params =
        ListParams::default().labels(&format!("fluidbg.io/blue-green-ref={blue_green_ref}"));

    for attempt in 1..=60 {
        let deployment_count = deployments.list(&params).await?.items.len();
        let service_count = services.list(&params).await?.items.len();
        let config_map_count = config_maps.list(&params).await?.items.len();
        let pod_count = pods.list(&params).await?.items.len();
        if deployment_count == 0 && service_count == 0 && config_map_count == 0 && pod_count == 0 {
            return Ok(());
        }
        info!(
            "waiting for inception resources for BGD '{}' to disappear: deployments={} services={} configmaps={} pods={} ({}/60)",
            blue_green_ref, deployment_count, service_count, config_map_count, pod_count, attempt
        );
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }

    Err(ReconcileError::Store(format!(
        "inception resources for BGD '{blue_green_ref}' were not deleted in namespace {namespace}"
    )))
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
