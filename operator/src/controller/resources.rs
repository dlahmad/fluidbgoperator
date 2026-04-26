use std::collections::BTreeMap;

use k8s_openapi::api::apps::v1::{Deployment, DeploymentSpec};
use k8s_openapi::api::core::v1::{
    Container, ContainerPort, EnvVar, PodSpec, PodTemplateSpec, Service, ServicePort, ServiceSpec,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::api::{Api, DeleteParams, Patch, PatchParams, PostParams};
use kube::api::{ApiResource, DynamicObject, GroupVersionKind};
use serde_json::Value;
use tracing::info;

use crate::crd::blue_green::{BlueGreenDeployment, InceptionPoint, ManagedDeploymentSpec};

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

async fn apply_dynamic_manifest(
    client: &kube::Client,
    default_namespace: &str,
    manifest: &Value,
) -> std::result::Result<(), ReconcileError> {
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
    let ar = ApiResource::from_gvk(&gvk);
    let mut object: DynamicObject = serde_json::from_value(manifest.clone()).map_err(|err| {
        ReconcileError::Store(format!(
            "failed to deserialize dynamic resource {api_version}/{kind}/{name}: {err}"
        ))
    })?;
    object
        .metadata
        .namespace
        .get_or_insert_with(|| namespace.to_string());
    let api: Api<DynamicObject> = Api::namespaced_with(client.clone(), namespace, &ar);
    let pp = PatchParams::apply("fluidbg-operator").force();
    api.patch(name, &pp, &Patch::Apply(&object)).await?;
    Ok(())
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

async fn delete_resource<K>(
    api: Api<K>,
    kind: &str,
    namespace: &str,
    name: &str,
) -> std::result::Result<(), ReconcileError>
where
    K: kube::Resource<DynamicType = ()>
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
