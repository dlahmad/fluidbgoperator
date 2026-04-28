use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use fluidbg_operator::crd::blue_green::BlueGreenDeployment;
use fluidbg_operator::crd::inception_plugin::InceptionPlugin;
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::{ConfigMap, Namespace, Pod, Secret, Service, ServiceAccount};
use k8s_openapi::api::rbac::v1::{ClusterRole, ClusterRoleBinding};
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kube::api::{
    ApiResource, DeleteParams, DynamicObject, GroupVersionKind, ListParams, Patch, PatchParams,
};
use kube::{Api, Client, ResourceExt};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::command;
use crate::config::E2eConfig;

const APPLY_MANAGER: &str = "fluidbg-e2e";

#[derive(Clone)]
pub struct Kube {
    client: Client,
}

pub struct EnvPairExpectation<'a> {
    pub first: &'a str,
    pub second: &'a str,
    pub namespace: &'a str,
    pub env_name: &'a str,
    pub expected_a: &'a str,
    pub expected_b: &'a str,
    pub forbidden: &'a str,
}

impl Kube {
    pub async fn new() -> Result<Self> {
        Ok(Self {
            client: Client::try_default()
                .await
                .context("failed to create Kubernetes client from current kubeconfig")?,
        })
    }

    pub fn client(&self) -> Client {
        self.client.clone()
    }

    pub async fn apply_namespace(&self, namespace: &str) -> Result<()> {
        let api: Api<Namespace> = Api::all(self.client.clone());
        let value = serde_json::json!({
            "apiVersion": "v1",
            "kind": "Namespace",
            "metadata": { "name": namespace }
        });
        apply_typed(&api, namespace, &value).await
    }

    pub async fn apply_file(&self, path: &str) -> Result<()> {
        let contents = std::fs::read_to_string(path).with_context(|| format!("read {path}"))?;
        for document in yaml_documents(&contents) {
            self.apply_document(document)
                .await
                .with_context(|| format!("apply document from {path}"))?;
        }
        Ok(())
    }

    async fn apply_document(&self, document: &str) -> Result<()> {
        let value: Value = serde_yaml_ng::from_str(document).context("parse Kubernetes YAML")?;
        let kind = value
            .get("kind")
            .and_then(Value::as_str)
            .context("manifest has no kind")?;
        let name = metadata_string(&value, "name")?;
        let namespace = metadata_string(&value, "namespace").ok();

        match kind {
            "Namespace" => {
                let api: Api<Namespace> = Api::all(self.client.clone());
                apply_typed(&api, &name, &value).await
            }
            "Deployment" => {
                let api: Api<Deployment> =
                    Api::namespaced(self.client.clone(), namespace_required(kind, &namespace)?);
                apply_typed(&api, &name, &value).await
            }
            "Service" => {
                let api: Api<Service> =
                    Api::namespaced(self.client.clone(), namespace_required(kind, &namespace)?);
                apply_typed(&api, &name, &value).await
            }
            "Secret" => {
                let api: Api<Secret> =
                    Api::namespaced(self.client.clone(), namespace_required(kind, &namespace)?);
                apply_typed(&api, &name, &value).await
            }
            "ConfigMap" => {
                let api: Api<ConfigMap> =
                    Api::namespaced(self.client.clone(), namespace_required(kind, &namespace)?);
                apply_typed(&api, &name, &value).await
            }
            "BlueGreenDeployment" => {
                let api: Api<BlueGreenDeployment> =
                    Api::namespaced(self.client.clone(), namespace_required(kind, &namespace)?);
                apply_typed(&api, &name, &value).await
            }
            "InceptionPlugin" => {
                let api: Api<InceptionPlugin> =
                    Api::namespaced(self.client.clone(), namespace_required(kind, &namespace)?);
                apply_typed(&api, &name, &value).await
            }
            other => bail!("unsupported manifest kind in Rust e2e apply: {other}"),
        }
    }

    pub async fn delete_crds(&self) -> Result<()> {
        let api: Api<CustomResourceDefinition> = Api::all(self.client.clone());
        delete_if_exists(&api, "bluegreendeployments.fluidbg.io").await?;
        delete_if_exists(&api, "inceptionplugins.fluidbg.io").await?;
        self.wait_deleted(
            "crd",
            "bluegreendeployments.fluidbg.io",
            "",
            Duration::from_secs(90),
        )
        .await?;
        self.wait_deleted(
            "crd",
            "inceptionplugins.fluidbg.io",
            "",
            Duration::from_secs(90),
        )
        .await
    }

    pub async fn rollout_status(
        &self,
        deployment: &str,
        namespace: &str,
        timeout: Duration,
    ) -> Result<()> {
        let api: Api<Deployment> = Api::namespaced(self.client.clone(), namespace);
        wait_until(timeout, Duration::from_secs(2), || async {
            let Ok(deploy) = api.get(deployment).await else {
                return false;
            };
            deployment_available(&deploy)
        })
        .await
        .with_context(|| format!("deployment/{deployment} did not roll out in {namespace}"))
    }

    pub async fn bgd(&self, name: &str, namespace: &str) -> Result<BlueGreenDeployment> {
        let api: Api<BlueGreenDeployment> = Api::namespaced(self.client.clone(), namespace);
        api.get(name)
            .await
            .with_context(|| format!("get bluegreendeployment/{name} in {namespace}"))
    }

    pub async fn bgd_json(&self, name: &str, namespace: &str) -> Result<Value> {
        serde_json::to_value(self.bgd(name, namespace).await?).context("serialize BGD status")
    }

    pub async fn wait_bgd_generated_name(&self, bgd: &str, namespace: &str) -> Result<String> {
        wait_for_value(Duration::from_secs(60), Duration::from_secs(1), || async {
            self.bgd(bgd, namespace).await.ok().and_then(|bgd| {
                bgd.status
                    .and_then(|status| status.generated_deployment_name)
            })
        })
        .await
        .with_context(|| format!("bluegreendeployment/{bgd} did not publish generated name"))
    }

    pub async fn wait_bgd_phase(
        &self,
        bgd: &str,
        namespace: &str,
        expected: &str,
        attempts: u32,
    ) -> Result<()> {
        for i in 1..=attempts {
            let phase = self
                .bgd(bgd, namespace)
                .await
                .ok()
                .and_then(|bgd| bgd.status.and_then(|status| status.phase))
                .map(|phase| format!("{phase:?}"))
                .unwrap_or_default();
            if phase == expected {
                return Ok(());
            }
            eprintln!(
                "waiting for bluegreendeployment/{bgd} phase {expected}, current={phase:?} ({i}/{attempts})"
            );
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        bail!("bluegreendeployment/{bgd} did not reach phase {expected}")
    }

    pub async fn wait_inception_deployment_name(
        &self,
        bgd: &str,
        inception_point: &str,
        namespace: &str,
    ) -> Result<String> {
        self.wait_labeled_deployment_name(
            namespace,
            &format!(
                "fluidbg.io/blue-green-ref={bgd},fluidbg.io/inception-point={inception_point}"
            ),
            &format!("inception deployment bgd={bgd} point={inception_point}"),
        )
        .await
    }

    pub async fn wait_test_deployment_name(
        &self,
        bgd: &str,
        test: &str,
        namespace: &str,
    ) -> Result<String> {
        self.wait_labeled_deployment_name(
            namespace,
            &format!("fluidbg.io/blue-green-ref={bgd},fluidbg.io/test={test}"),
            &format!("test deployment bgd={bgd} test={test}"),
        )
        .await
    }

    pub async fn wait_labeled_deployment_name(
        &self,
        namespace: &str,
        selector: &str,
        label: &str,
    ) -> Result<String> {
        let api: Api<Deployment> = Api::namespaced(self.client.clone(), namespace);
        wait_for_value(Duration::from_secs(60), Duration::from_secs(1), || async {
            api.list(&ListParams::default().labels(selector))
                .await
                .ok()
                .and_then(|list| list.items.into_iter().next().map(|item| item.name_any()))
        })
        .await
        .with_context(|| format!("{label} not found in namespace {namespace}"))
    }

    pub async fn first_pod_name_by_selector(
        &self,
        namespace: &str,
        selector: &str,
    ) -> Result<String> {
        let api: Api<Pod> = Api::namespaced(self.client.clone(), namespace);
        wait_for_value(Duration::from_secs(60), Duration::from_secs(1), || async {
            api.list(&ListParams::default().labels(selector))
                .await
                .ok()
                .and_then(|list| list.items.into_iter().next().map(|item| item.name_any()))
        })
        .await
        .with_context(|| format!("pod with selector {selector} not found in namespace {namespace}"))
    }

    pub async fn wait_exists(
        &self,
        resource: &str,
        name: &str,
        namespace: &str,
        timeout: Duration,
    ) -> Result<()> {
        wait_until(timeout, Duration::from_secs(1), || async {
            self.exists(resource, name, namespace).await
        })
        .await
        .with_context(|| format!("{resource}/{name} was not created in namespace {namespace}"))
    }

    pub async fn wait_deleted(
        &self,
        resource: &str,
        name: &str,
        namespace: &str,
        timeout: Duration,
    ) -> Result<()> {
        wait_until(timeout, Duration::from_secs(1), || async {
            !self.exists(resource, name, namespace).await
        })
        .await
        .with_context(|| format!("{resource}/{name} was not deleted in namespace {namespace}"))
    }

    pub async fn exists(&self, resource: &str, name: &str, namespace: &str) -> bool {
        match resource {
            "deployment" => Api::<Deployment>::namespaced(self.client.clone(), namespace)
                .get_opt(name)
                .await
                .ok()
                .flatten()
                .is_some(),
            "service" => Api::<Service>::namespaced(self.client.clone(), namespace)
                .get_opt(name)
                .await
                .ok()
                .flatten()
                .is_some(),
            "serviceaccount" => Api::<ServiceAccount>::namespaced(self.client.clone(), namespace)
                .get_opt(name)
                .await
                .ok()
                .flatten()
                .is_some(),
            "secret" => Api::<Secret>::namespaced(self.client.clone(), namespace)
                .get_opt(name)
                .await
                .ok()
                .flatten()
                .is_some(),
            "configmap" => Api::<ConfigMap>::namespaced(self.client.clone(), namespace)
                .get_opt(name)
                .await
                .ok()
                .flatten()
                .is_some(),
            "pod" => Api::<Pod>::namespaced(self.client.clone(), namespace)
                .get_opt(name)
                .await
                .ok()
                .flatten()
                .is_some(),
            "bluegreendeployment" => {
                Api::<BlueGreenDeployment>::namespaced(self.client.clone(), namespace)
                    .get_opt(name)
                    .await
                    .ok()
                    .flatten()
                    .is_some()
            }
            "inceptionplugin" => Api::<InceptionPlugin>::namespaced(self.client.clone(), namespace)
                .get_opt(name)
                .await
                .ok()
                .flatten()
                .is_some(),
            "clusterrole" => Api::<ClusterRole>::all(self.client.clone())
                .get_opt(name)
                .await
                .ok()
                .flatten()
                .is_some(),
            "clusterrolebinding" => Api::<ClusterRoleBinding>::all(self.client.clone())
                .get_opt(name)
                .await
                .ok()
                .flatten()
                .is_some(),
            "crd" => Api::<CustomResourceDefinition>::all(self.client.clone())
                .get_opt(name)
                .await
                .ok()
                .flatten()
                .is_some(),
            _ => false,
        }
    }

    pub async fn wait_no_inception_resources(&self, namespace: &str) -> Result<()> {
        self.wait_label_resource_count(
            namespace,
            "fluidbg.io/inception-point",
            0,
            Duration::from_secs(60),
        )
        .await
    }

    pub async fn wait_no_blue_green_ref_resources(&self, namespace: &str, bgd: &str) -> Result<()> {
        self.wait_label_resource_count(
            namespace,
            &format!("fluidbg.io/blue-green-ref={bgd}"),
            0,
            Duration::from_secs(90),
        )
        .await
    }

    pub async fn wait_label_resource_count(
        &self,
        namespace: &str,
        selector: &str,
        expected: usize,
        timeout: Duration,
    ) -> Result<()> {
        wait_until(timeout, Duration::from_secs(1), || async {
            self.label_resource_count(namespace, selector)
                .await
                .unwrap_or(usize::MAX)
                == expected
        })
        .await
        .with_context(|| {
            format!(
                "resources with selector {selector} did not reach count={expected} in namespace {namespace}"
            )
        })
    }

    pub async fn label_resource_count(&self, namespace: &str, selector: &str) -> Result<usize> {
        let lp = ListParams::default().labels(selector);
        let deployments = Api::<Deployment>::namespaced(self.client.clone(), namespace)
            .list(&lp)
            .await?
            .items
            .len();
        let services = Api::<Service>::namespaced(self.client.clone(), namespace)
            .list(&lp)
            .await?
            .items
            .len();
        let configmaps = Api::<ConfigMap>::namespaced(self.client.clone(), namespace)
            .list(&lp)
            .await?
            .items
            .len();
        let secrets = Api::<Secret>::namespaced(self.client.clone(), namespace)
            .list(&lp)
            .await?
            .items
            .len();
        let pods = Api::<Pod>::namespaced(self.client.clone(), namespace)
            .list(&lp)
            .await?
            .items
            .len();
        Ok(deployments + services + configmaps + secrets + pods)
    }

    pub async fn delete_labeled_resources(&self, namespace: &str, selector: &str) -> Result<()> {
        let lp = ListParams::default().labels(selector);
        delete_labeled(
            Api::<Deployment>::namespaced(self.client.clone(), namespace),
            &lp,
        )
        .await?;
        delete_labeled(
            Api::<Service>::namespaced(self.client.clone(), namespace),
            &lp,
        )
        .await?;
        delete_labeled(
            Api::<ConfigMap>::namespaced(self.client.clone(), namespace),
            &lp,
        )
        .await?;
        delete_labeled(
            Api::<Secret>::namespaced(self.client.clone(), namespace),
            &lp,
        )
        .await?;
        delete_labeled(Api::<Job>::namespaced(self.client.clone(), namespace), &lp).await?;
        delete_labeled(Api::<Pod>::namespaced(self.client.clone(), namespace), &lp).await
    }

    pub async fn delete_named(&self, resource: &str, name: &str, namespace: &str) -> Result<()> {
        match resource {
            "deployment" => {
                delete_if_exists(
                    &Api::<Deployment>::namespaced(self.client.clone(), namespace),
                    name,
                )
                .await
            }
            "service" => {
                delete_if_exists(
                    &Api::<Service>::namespaced(self.client.clone(), namespace),
                    name,
                )
                .await
            }
            "secret" => {
                delete_if_exists(
                    &Api::<Secret>::namespaced(self.client.clone(), namespace),
                    name,
                )
                .await
            }
            "configmap" => {
                delete_if_exists(
                    &Api::<ConfigMap>::namespaced(self.client.clone(), namespace),
                    name,
                )
                .await
            }
            "job" => {
                delete_if_exists(
                    &Api::<Job>::namespaced(self.client.clone(), namespace),
                    name,
                )
                .await
            }
            "inceptionplugin" => {
                delete_if_exists(
                    &Api::<InceptionPlugin>::namespaced(self.client.clone(), namespace),
                    name,
                )
                .await
            }
            "bluegreendeployment" => {
                delete_if_exists(
                    &Api::<BlueGreenDeployment>::namespaced(self.client.clone(), namespace),
                    name,
                )
                .await
            }
            other => bail!("unsupported delete resource {other}"),
        }
    }

    pub async fn cleanup_stale_blue_green_deployments(&self) -> Result<()> {
        if !self
            .exists("crd", "bluegreendeployments.fluidbg.io", "")
            .await
        {
            return Ok(());
        }
        let api: Api<DynamicObject> = Api::all_with(
            self.client.clone(),
            &ApiResource::from_gvk(&GroupVersionKind::gvk(
                "fluidbg.io",
                "v1alpha1",
                "BlueGreenDeployment",
            )),
        );
        for bgd in api.list(&ListParams::default()).await?.items {
            let Some(namespace) = bgd.namespace() else {
                continue;
            };
            let name = bgd.name_any();
            let namespaced: Api<DynamicObject> = Api::namespaced_with(
                self.client.clone(),
                &namespace,
                &ApiResource::from_gvk(&GroupVersionKind::gvk(
                    "fluidbg.io",
                    "v1alpha1",
                    "BlueGreenDeployment",
                )),
            );
            let patch = serde_json::json!({ "metadata": { "finalizers": [] } });
            let _ = namespaced
                .patch(&name, &PatchParams::default(), &Patch::Merge(&patch))
                .await;
            let _ = namespaced.delete(&name, &DeleteParams::default()).await;
        }
        Ok(())
    }

    pub async fn force_delete_bgd(&self, name: &str, namespace: &str) -> Result<()> {
        let api: Api<BlueGreenDeployment> = Api::namespaced(self.client.clone(), namespace);
        let patch = serde_json::json!({ "metadata": { "finalizers": [] } });
        api.patch(name, &PatchParams::default(), &Patch::Merge(&patch))
            .await?;
        delete_if_exists(&api, name).await
    }

    pub async fn wait_deployment_label(
        &self,
        deployment: &str,
        namespace: &str,
        label_key: &str,
        expected: &str,
    ) -> Result<()> {
        let api: Api<Deployment> = Api::namespaced(self.client.clone(), namespace);
        wait_until(Duration::from_secs(60), Duration::from_secs(1), || async {
            api.get(deployment)
                .await
                .ok()
                .and_then(|deployment| deployment.labels().get(label_key).cloned())
                .as_deref()
                == Some(expected)
        })
        .await
        .with_context(|| {
            format!("deployment/{deployment} did not reach label {label_key}={expected}")
        })
    }

    pub async fn wait_deployment_replicas(
        &self,
        deployment: &str,
        namespace: &str,
        expected: i32,
    ) -> Result<()> {
        let api: Api<Deployment> = Api::namespaced(self.client.clone(), namespace);
        wait_until(Duration::from_secs(120), Duration::from_secs(2), || async {
            let Ok(deployment) = api.get(deployment).await else {
                return false;
            };
            let desired = deployment
                .spec
                .as_ref()
                .and_then(|spec| spec.replicas)
                .unwrap_or(1);
            let available = deployment
                .status
                .as_ref()
                .and_then(|status| status.available_replicas)
                .unwrap_or(0);
            desired == expected && available == expected
        })
        .await
        .with_context(|| format!("deployment/{deployment} did not reach replicas={expected}"))
    }

    pub async fn deployment_env_value(
        &self,
        deployment: &str,
        namespace: &str,
        env_name: &str,
    ) -> Result<Option<String>> {
        let api: Api<Deployment> = Api::namespaced(self.client.clone(), namespace);
        let deployment = api.get(deployment).await?;
        let containers = deployment
            .spec
            .as_ref()
            .and_then(|spec| spec.template.spec.as_ref())
            .map(|spec| spec.containers.as_slice())
            .unwrap_or(&[]);
        for container in containers {
            for env in container.env.as_deref().unwrap_or(&[]) {
                if env.name == env_name {
                    return Ok(env.value.clone());
                }
            }
        }
        Ok(None)
    }

    pub async fn wait_deployment_env_pair_values(
        &self,
        expected: EnvPairExpectation<'_>,
    ) -> Result<()> {
        wait_until(Duration::from_secs(60), Duration::from_secs(1), || async {
            let first_value = self
                .deployment_env_value(expected.first, expected.namespace, expected.env_name)
                .await
                .ok()
                .flatten();
            let second_value = self
                .deployment_env_value(expected.second, expected.namespace, expected.env_name)
                .await
                .ok()
                .flatten();
            let Some(first_value) = first_value else {
                return false;
            };
            let Some(second_value) = second_value else {
                return false;
            };
            first_value != expected.forbidden
                && second_value != expected.forbidden
                && first_value != second_value
                && ((first_value == expected.expected_a && second_value == expected.expected_b)
                    || (first_value == expected.expected_b && second_value == expected.expected_a))
        })
        .await
        .with_context(|| {
            format!(
                "deployments/{},{} did not reach distinct env {} values",
                expected.first, expected.second, expected.env_name
            )
        })
    }

    pub async fn get_inception_config_value(
        &self,
        bgd: &str,
        inception_point: &str,
        namespace: &str,
        path: &str,
    ) -> Result<String> {
        let api: Api<ConfigMap> = Api::namespaced(self.client.clone(), namespace);
        let list = api
            .list(&ListParams::default().labels(&format!(
                "fluidbg.io/blue-green-ref={bgd},fluidbg.io/inception-point={inception_point}"
            )))
            .await?;
        let config = list
            .items
            .first()
            .and_then(|configmap| configmap.data.as_ref())
            .and_then(|data| data.get("config.yaml"))
            .context("config map does not contain config.yaml")?;
        yaml_value_at_path(config, path)
    }

    pub async fn reset_deployment(
        &self,
        namespace: &str,
        deployment: &str,
        app_label: &str,
    ) -> Result<()> {
        let api: Api<Deployment> = Api::namespaced(self.client.clone(), namespace);
        let patch = serde_json::json!({ "spec": { "replicas": 0 } });
        api.patch(deployment, &PatchParams::default(), &Patch::Merge(&patch))
            .await?;
        self.wait_label_resource_count(
            namespace,
            &format!("app={app_label}"),
            0,
            Duration::from_secs(60),
        )
        .await?;
        let patch = serde_json::json!({ "spec": { "replicas": 1 } });
        api.patch(deployment, &PatchParams::default(), &Patch::Merge(&patch))
            .await?;
        Ok(())
    }

    pub fn install_operator_chart(&self, config: &E2eConfig) -> Result<()> {
        install_operator_chart(config)
    }

    pub async fn postgres_case_rows(
        &self,
        system_namespace: &str,
        bgd_namespace: &str,
        bgd: &str,
    ) -> Result<String> {
        command::output(
            "kubectl",
            [
                "exec",
                "-n",
                system_namespace,
                "deploy/postgres",
                "--",
                "env",
                "PGPASSWORD=fluidbg",
                "psql",
                "-U",
                "fluidbg",
                "-d",
                "fluidbg",
                "-tAc",
                &format!(
                    "select count(*) from fluidbg_cases where blue_green_ref='{}/{}';",
                    bgd_namespace, bgd
                ),
            ],
        )
        .map(|value| value.chars().filter(|ch| !ch.is_whitespace()).collect())
    }
}

pub fn yaml_value_at_path(yaml: &str, path: &str) -> Result<String> {
    let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(yaml)?;
    let mut current = &value;
    for segment in path.split('.') {
        current = current
            .get(segment)
            .ok_or_else(|| anyhow!("path segment {segment} not found in {path}"))?;
    }
    match current {
        serde_yaml_ng::Value::String(value) => Ok(value.clone()),
        serde_yaml_ng::Value::Number(value) => Ok(value.to_string()),
        serde_yaml_ng::Value::Bool(value) => Ok(value.to_string()),
        other => bail!("path {path} does not resolve to scalar: {other:?}"),
    }
}

pub async fn wait_until<F, Fut>(
    timeout: Duration,
    interval: Duration,
    mut predicate: F,
) -> Result<()>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if predicate().await {
            return Ok(());
        }
        tokio::time::sleep(interval).await;
    }
    bail!("timed out waiting for condition")
}

pub async fn wait_for_value<T, F, Fut>(
    timeout: Duration,
    interval: Duration,
    mut provider: F,
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(value) = provider().await {
            return Ok(value);
        }
        tokio::time::sleep(interval).await;
    }
    bail!("timed out waiting for value")
}

fn yaml_documents(contents: &str) -> impl Iterator<Item = &str> {
    contents
        .split("\n---")
        .map(str::trim)
        .filter(|document| !document.is_empty())
}

async fn apply_typed<K>(api: &Api<K>, name: &str, value: &Value) -> Result<()>
where
    K: Clone + DeserializeOwned + Serialize + std::fmt::Debug,
{
    let params = PatchParams::apply(APPLY_MANAGER).force();
    api.patch(name, &params, &Patch::Apply(value))
        .await
        .with_context(|| format!("server-side apply {}", name))?;
    Ok(())
}

async fn delete_if_exists<K>(api: &Api<K>, name: &str) -> Result<()>
where
    K: Clone + DeserializeOwned + std::fmt::Debug,
{
    match api.delete(name, &DeleteParams::default()).await {
        Ok(_) => Ok(()),
        Err(kube::Error::Api(error)) if error.code == 404 => Ok(()),
        Err(error) => Err(error).with_context(|| format!("delete {name}")),
    }
}

async fn delete_labeled<K>(api: Api<K>, lp: &ListParams) -> Result<()>
where
    K: Clone + DeserializeOwned + ResourceExt + std::fmt::Debug,
{
    for item in api.list(lp).await?.items {
        let _ = api.delete(&item.name_any(), &DeleteParams::default()).await;
    }
    Ok(())
}

fn deployment_available(deployment: &Deployment) -> bool {
    let desired = deployment
        .spec
        .as_ref()
        .and_then(|spec| spec.replicas)
        .unwrap_or(1);
    let generation = deployment.metadata.generation.unwrap_or_default();
    let Some(status) = deployment.status.as_ref() else {
        return false;
    };
    status.observed_generation.unwrap_or_default() >= generation
        && status.updated_replicas.unwrap_or_default() >= desired
        && status.available_replicas.unwrap_or_default() >= desired
}

fn metadata_string(value: &Value, key: &str) -> Result<String> {
    value
        .pointer(&format!("/metadata/{key}"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .with_context(|| format!("manifest metadata.{key} is missing"))
}

fn namespace_required<'a>(kind: &str, namespace: &'a Option<String>) -> Result<&'a str> {
    namespace
        .as_deref()
        .with_context(|| format!("{kind} manifest is missing metadata.namespace"))
}

fn install_operator_chart(config: &E2eConfig) -> Result<()> {
    let chart = config.root_dir.join("charts/fluidbg-operator");
    let mut args = vec![
        "upgrade".to_string(),
        "--install".to_string(),
        "fluidbg-e2e".to_string(),
        chart.to_string_lossy().to_string(),
        "--namespace".to_string(),
        config.system_namespace.clone(),
        "--create-namespace".to_string(),
        "--set".to_string(),
        "fullnameOverride=fluidbg-operator".to_string(),
        "--set".to_string(),
        "serviceAccount.name=fluidbg-operator".to_string(),
        "--set".to_string(),
        format!("operator.replicaCount={}", config.operator_replicas),
        "--set".to_string(),
        "operator.image.repository=fluidbg/fbg-operator".to_string(),
        "--set".to_string(),
        format!("operator.image.tag={}", config.image_tag),
        "--set".to_string(),
        "operator.image.pullPolicy=Never".to_string(),
        "--set".to_string(),
        "operator.orphanCleanup.intervalSeconds=5".to_string(),
        "--set".to_string(),
        "operator.auth.createSigningSecret=true".to_string(),
        "--set".to_string(),
        format!("operator.auth.signingSecretNamespace={}", config.system_namespace),
        "--set".to_string(),
        "operator.auth.signingSecretName=fluidbg-e2e-auth".to_string(),
        "--set".to_string(),
        "operator.auth.signingSecretKey=signing-key".to_string(),
        "--set-string".to_string(),
        "operator.auth.signingSecretValue=fluidbg-e2e-signing-key".to_string(),
        "--set".to_string(),
        format!("builtinPlugins.namespaces[0]={}", config.namespace),
        "--set".to_string(),
        "builtinPlugins.http.image.repository=fluidbg/fbg-plugin-http".to_string(),
        "--set".to_string(),
        format!("builtinPlugins.http.image.tag={}", config.image_tag),
        "--set".to_string(),
        "builtinPlugins.rabbitmq.image.repository=fluidbg/fbg-plugin-rabbitmq".to_string(),
        "--set".to_string(),
        format!("builtinPlugins.rabbitmq.image.tag={}", config.image_tag),
        "--set".to_string(),
        "builtinPlugins.rabbitmq.manager.enabled=true".to_string(),
        "--set".to_string(),
        "builtinPlugins.rabbitmq.manager.amqpUrl=amqp://fluidbg:fluidbg@rabbitmq.fluidbg-system:5672/%2f".to_string(),
        "--set".to_string(),
        "builtinPlugins.rabbitmq.manager.managementUrl=http://rabbitmq.fluidbg-system:15672".to_string(),
        "--set".to_string(),
        "builtinPlugins.rabbitmq.manager.managementUsername=fluidbg".to_string(),
        "--set".to_string(),
        "builtinPlugins.rabbitmq.manager.managementPassword=fluidbg".to_string(),
        "--set".to_string(),
        "builtinPlugins.rabbitmq.manager.managementVhost=/".to_string(),
        "--set".to_string(),
        "builtinPlugins.azureServiceBus.enabled=false".to_string(),
    ];
    if config.state_store == crate::config::StateStore::Postgres {
        args.extend([
            "--set".to_string(),
            "stateStore.type=postgres".to_string(),
            "--set".to_string(),
            "stateStore.postgres.authMode=password".to_string(),
            "--set".to_string(),
            "stateStore.postgres.urlSecretName=fluidbg-postgres".to_string(),
            "--set".to_string(),
            "stateStore.postgres.urlSecretKey=url".to_string(),
            "--set".to_string(),
            "stateStore.postgres.tableName=fluidbg_cases".to_string(),
        ]);
    }
    command::run("helm", args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yaml_path_reads_nested_scalar_values() {
        let yaml = r#"
duplicator:
  greenInputQueue: orders-green
  blueInputQueue: orders-blue
queueDeclaration:
  durable: true
"#;

        assert_eq!(
            yaml_value_at_path(yaml, "duplicator.greenInputQueue").unwrap(),
            "orders-green"
        );
        assert_eq!(
            yaml_value_at_path(yaml, "queueDeclaration.durable").unwrap(),
            "true"
        );
    }
}
