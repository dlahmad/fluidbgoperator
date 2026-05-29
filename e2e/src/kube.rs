use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use fluidbg_operator::crd::blue_green::BlueGreenDeployment;
use fluidbg_operator::crd::inception_plugin::InceptionPlugin;
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::{
    ConfigMap, Namespace, Node, Pod, Secret, Service, ServiceAccount,
};
use k8s_openapi::api::rbac::v1::{ClusterRole, ClusterRoleBinding};
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kube::api::{
    ApiResource, AttachParams, DeleteParams, DynamicObject, GroupVersionKind, ListParams, Patch,
    PatchParams,
};
use kube::{Api, Client, ResourceExt};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

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

pub struct PodHttpRequest<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub basic_auth: Option<(&'a str, &'a str)>,
    pub headers: Vec<(&'a str, &'a str)>,
    pub body: Option<Value>,
}

pub struct PodHttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
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

    pub async fn apply_secret_string_data(
        &self,
        namespace: &str,
        name: &str,
        data: serde_json::Map<String, Value>,
    ) -> Result<()> {
        let api: Api<Secret> = Api::namespaced(self.client.clone(), namespace);
        let value = serde_json::json!({
            "apiVersion": "v1",
            "kind": "Secret",
            "metadata": { "name": name, "namespace": namespace },
            "type": "Opaque",
            "stringData": data,
        });
        apply_typed(&api, name, &value).await
    }

    pub async fn apply_configmap_data(
        &self,
        namespace: &str,
        name: &str,
        data: serde_json::Map<String, Value>,
    ) -> Result<()> {
        let api: Api<ConfigMap> = Api::namespaced(self.client.clone(), namespace);
        let value = serde_json::json!({
            "apiVersion": "v1",
            "kind": "ConfigMap",
            "metadata": { "name": name, "namespace": namespace },
            "data": data,
        });
        apply_typed(&api, name, &value).await
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
                let api: Api<InceptionPlugin> = Api::all(self.client.clone());
                apply_typed(&api, &name, &value).await
            }
            other => bail!("unsupported manifest kind in Rust e2e apply: {other}"),
        }
    }

    pub async fn delete_crds(&self) -> Result<()> {
        self.cleanup_stale_blue_green_deployments().await?;
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

    pub async fn pod_http_json_by_selector(
        &self,
        namespace: &str,
        selector: &str,
        port: u16,
        request: PodHttpRequest<'_>,
    ) -> Result<Value> {
        let response = self
            .pod_http_by_selector(namespace, selector, port, request)
            .await?;
        if !(200..300).contains(&response.status) {
            bail!("HTTP request failed with status {}", response.status);
        }
        serde_json::from_slice(trim_ascii_whitespace(&response.body))
            .context("HTTP response body is not JSON")
    }

    pub async fn pod_http_by_selector(
        &self,
        namespace: &str,
        selector: &str,
        port: u16,
        request: PodHttpRequest<'_>,
    ) -> Result<PodHttpResponse> {
        let pod = self
            .ready_pod_name_by_selector(namespace, selector, Duration::from_secs(30))
            .await?;
        self.pod_http(namespace, &pod, port, request).await
    }

    async fn ready_pod_name_by_selector(
        &self,
        namespace: &str,
        selector: &str,
        timeout: Duration,
    ) -> Result<String> {
        let api: Api<Pod> = Api::namespaced(self.client.clone(), namespace);
        wait_for_value(timeout, Duration::from_secs(1), || async {
            api.list(&ListParams::default().labels(selector))
                .await
                .ok()
                .and_then(|list| {
                    list.items
                        .into_iter()
                        .find(pod_ready)
                        .map(|item| item.name_any())
                })
        })
        .await
        .with_context(|| {
            format!("ready pod with selector {selector} not found in namespace {namespace}")
        })
    }

    async fn pod_http(
        &self,
        namespace: &str,
        pod: &str,
        port: u16,
        request: PodHttpRequest<'_>,
    ) -> Result<PodHttpResponse> {
        let api: Api<Pod> = Api::namespaced(self.client.clone(), namespace);
        let mut portforward = api
            .portforward(pod, &[port])
            .await
            .with_context(|| format!("open Kubernetes API port-forward to pod/{pod}:{port}"))?;
        let mut stream = portforward
            .take_stream(port)
            .with_context(|| format!("take port-forward stream for pod/{pod}:{port}"))?;
        let request_path = request.path;
        let request = render_http_request(request)?;
        stream
            .write_all(request.as_bytes())
            .await
            .with_context(|| format!("write HTTP request to pod/{pod}:{port}{request_path}"))?;
        let response = read_http_response(&mut stream, Duration::from_secs(10))
            .await
            .with_context(|| format!("read HTTP response from pod/{pod}:{port}{request_path}"))?;
        portforward.abort();
        parse_http_response(&response)
            .with_context(|| format!("parse HTTP response from pod/{pod}:{port}{request_path}"))
    }

    pub async fn pod_exec_stdout_by_selector<I, S>(
        &self,
        namespace: &str,
        selector: &str,
        command: I,
    ) -> Result<String>
    where
        I: IntoIterator<Item = S> + std::fmt::Debug,
        S: Into<String>,
    {
        let pod = self
            .ready_pod_name_by_selector(namespace, selector, Duration::from_secs(30))
            .await?;
        let api: Api<Pod> = Api::namespaced(self.client.clone(), namespace);
        let mut process = api
            .exec(&pod, command, &AttachParams::default())
            .await
            .with_context(|| format!("exec in pod/{pod}"))?;
        let mut stdout_reader = process.stdout().context("exec stdout was not attached")?;
        let mut stderr_reader = process.stderr().context("exec stderr was not attached")?;
        let status = process
            .take_status()
            .context("exec status was not attached")?;
        let mut stdout = String::new();
        let mut stderr = String::new();
        let (stdout_result, stderr_result, status) = tokio::join!(
            stdout_reader.read_to_string(&mut stdout),
            stderr_reader.read_to_string(&mut stderr),
            status,
        );
        stdout_result.context("read exec stdout")?;
        stderr_result.context("read exec stderr")?;
        if let Some(status) = status
            && status.status.as_deref() == Some("Failure")
        {
            bail!(
                "pod/{pod} exec failed: {}",
                status
                    .message
                    .or_else(|| (!stderr.trim().is_empty()).then(|| stderr.trim().to_string()))
                    .unwrap_or_else(|| "unknown failure".to_string())
            );
        }
        process.abort();
        Ok(stdout)
    }

    pub async fn first_node_arch(&self) -> Result<String> {
        let api: Api<Node> = Api::all(self.client.clone());
        let node = api
            .list(&ListParams::default().limit(1))
            .await?
            .items
            .into_iter()
            .next()
            .context("cluster has no nodes")?;
        node.status
            .and_then(|status| status.node_info)
            .map(|info| info.architecture)
            .context("first node has no architecture in status.nodeInfo")
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
            "inceptionplugin" => Api::<InceptionPlugin>::all(self.client.clone())
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
            "namespace" => Api::<Namespace>::all(self.client.clone())
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
                delete_if_exists(&Api::<InceptionPlugin>::all(self.client.clone()), name).await
            }
            "bluegreendeployment" => {
                delete_if_exists(
                    &Api::<BlueGreenDeployment>::namespaced(self.client.clone(), namespace),
                    name,
                )
                .await
            }
            "namespace" => {
                delete_if_exists(&Api::<Namespace>::all(self.client.clone()), name).await
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
        self.wait_no_blue_green_deployments(Duration::from_secs(90))
            .await
    }

    async fn wait_no_blue_green_deployments(&self, timeout: Duration) -> Result<()> {
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
        wait_until(timeout, Duration::from_secs(1), || {
            let api = api.clone();
            async move {
                api.list(&ListParams::default())
                    .await
                    .map(|list| list.items.is_empty())
                    .unwrap_or(false)
            }
        })
        .await
        .context("stale BlueGreenDeployment resources were not deleted")
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

    pub async fn deployment_pod_selector(
        &self,
        deployment: &str,
        namespace: &str,
    ) -> Result<String> {
        let api: Api<Deployment> = Api::namespaced(self.client.clone(), namespace);
        let deployment = api.get(deployment).await?;
        let labels = deployment
            .spec
            .and_then(|spec| spec.selector.match_labels)
            .context("deployment selector has no matchLabels")?;
        if labels.is_empty() {
            bail!("deployment selector has empty matchLabels");
        }
        Ok(labels
            .into_iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join(","))
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
        self.pod_exec_stdout_by_selector(
            system_namespace,
            "app=postgres",
            [
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
        .await
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
        let remaining = deadline.saturating_duration_since(Instant::now());
        let attempt_timeout = remaining.min(Duration::from_secs(10));
        if tokio::time::timeout(attempt_timeout, predicate())
            .await
            .unwrap_or(false)
        {
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
        let remaining = deadline.saturating_duration_since(Instant::now());
        let attempt_timeout = remaining.min(Duration::from_secs(10));
        if let Some(value) = tokio::time::timeout(attempt_timeout, provider())
            .await
            .ok()
            .flatten()
        {
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

fn pod_ready(pod: &Pod) -> bool {
    pod.status
        .as_ref()
        .and_then(|status| status.conditions.as_ref())
        .is_some_and(|conditions| {
            conditions.iter().any(|condition| {
                condition.type_ == "Ready" && condition.status.eq_ignore_ascii_case("True")
            })
        })
}

fn parse_http_response(response: &[u8]) -> Result<PodHttpResponse> {
    let text = String::from_utf8_lossy(response);
    let (head, body) = text
        .split_once("\r\n\r\n")
        .or_else(|| text.split_once("\n\n"))
        .context("HTTP response has no header/body separator")?;
    let status = head.lines().next().unwrap_or_default();
    let status = status
        .split_whitespace()
        .nth(1)
        .context("HTTP response has no status code")?
        .parse::<u16>()
        .context("HTTP response status code is not numeric")?;
    let body = if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        decode_chunked_body(body.as_bytes())?
    } else {
        body.as_bytes().to_vec()
    };
    Ok(PodHttpResponse { status, body })
}

async fn read_http_response<R>(stream: &mut R, timeout: Duration) -> Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let deadline = Instant::now() + timeout;
    let mut response = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        if http_response_complete(&response)? {
            return Ok(response);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            bail!("timed out before HTTP response completed");
        }
        let read = tokio::time::timeout(remaining, stream.read(&mut chunk))
            .await
            .context("timed out reading HTTP response")?
            .context("read HTTP response")?;
        if read == 0 {
            return Ok(response);
        }
        response.extend_from_slice(&chunk[..read]);
    }
}

fn http_response_complete(response: &[u8]) -> Result<bool> {
    let Some((header_end, separator_len)) = http_header_end(response) else {
        return Ok(false);
    };
    let headers = String::from_utf8_lossy(&response[..header_end]).to_ascii_lowercase();
    let body = &response[header_end + separator_len..];
    if headers.contains("transfer-encoding: chunked") {
        return Ok(decode_chunked_body(body).is_ok());
    }
    if let Some(length) = headers.lines().find_map(|line| {
        line.strip_prefix("content-length:")
            .and_then(|value| value.trim().parse::<usize>().ok())
    }) {
        return Ok(body.len() >= length);
    }
    Ok(false)
}

fn http_header_end(response: &[u8]) -> Option<(usize, usize)> {
    response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| (position, 4))
        .or_else(|| {
            response
                .windows(2)
                .position(|window| window == b"\n\n")
                .map(|position| (position, 2))
        })
}

fn render_http_request(request: PodHttpRequest<'_>) -> Result<String> {
    let body = request
        .body
        .map(|body| serde_json::to_vec(&body))
        .transpose()?
        .unwrap_or_default();
    let mut rendered = format!(
        "{} {} HTTP/1.1\r\nHost: localhost\r\nAccept: application/json\r\nConnection: close\r\n",
        request.method, request.path
    );
    if let Some((username, password)) = request.basic_auth {
        let encoded =
            base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"));
        rendered.push_str(&format!("Authorization: Basic {encoded}\r\n"));
    }
    for (name, value) in request.headers {
        rendered.push_str(&format!("{name}: {value}\r\n"));
    }
    if !body.is_empty() {
        rendered.push_str("Content-Type: application/json\r\n");
        rendered.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    rendered.push_str("\r\n");
    if !body.is_empty() {
        rendered.push_str(&String::from_utf8(body).context("HTTP JSON body is not UTF-8")?);
    }
    Ok(rendered)
}

fn decode_chunked_body(body: &[u8]) -> Result<Vec<u8>> {
    let mut cursor = body;
    let mut decoded = Vec::new();
    loop {
        let Some(line_end) = cursor.windows(2).position(|window| window == b"\r\n") else {
            bail!("chunked body is missing chunk header terminator");
        };
        let size_text = std::str::from_utf8(&cursor[..line_end])?;
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or_default(), 16)
            .context("invalid chunk size")?;
        cursor = &cursor[line_end + 2..];
        if size == 0 {
            return Ok(decoded);
        }
        if cursor.len() < size + 2 {
            bail!("chunked body ended before declared chunk size");
        }
        decoded.extend_from_slice(&cursor[..size]);
        cursor = &cursor[size + 2..];
    }
}

fn trim_ascii_whitespace(value: &[u8]) -> &[u8] {
    let start = value
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(value.len());
    let end = value
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map(|idx| idx + 1)
        .unwrap_or(start);
    &value[start..end]
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
        format!(
            "operator.auth.signingSecretNamespace={}",
            config.system_namespace
        ),
        "--set".to_string(),
        "operator.auth.signingSecretName=fluidbg-e2e-auth".to_string(),
        "--set".to_string(),
        "operator.auth.signingSecretKey=signing-key".to_string(),
        "--set-string".to_string(),
        "operator.auth.signingSecretValue=fluidbg-e2e-signing-key".to_string(),
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
        rabbitmq_manager_amqp_url(config),
        "--set".to_string(),
        rabbitmq_manager_management_url(config),
        "--set".to_string(),
        format!(
            "builtinPlugins.rabbitmq.manager.managementAllowInsecure={}",
            !config.full_tls
        ),
        "--set".to_string(),
        "builtinPlugins.rabbitmq.manager.managementUsername=fluidbg".to_string(),
        "--set".to_string(),
        "builtinPlugins.rabbitmq.manager.managementPassword=fluidbg".to_string(),
        "--set".to_string(),
        "builtinPlugins.rabbitmq.manager.managementVhost=/".to_string(),
        "--set".to_string(),
        "builtinPlugins.azureServiceBus.enabled=false".to_string(),
    ];
    let tls_values_file = if config.full_tls {
        let path = write_tls_values_file()?;
        args.extend(["-f".to_string(), path.to_string_lossy().to_string()]);
        Some(path)
    } else {
        None
    };
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
    let result = command::run("helm", args);
    if let Some(path) = tls_values_file {
        let _ = std::fs::remove_file(path);
    }
    result
}

fn rabbitmq_manager_amqp_url(config: &E2eConfig) -> String {
    if config.full_tls {
        "builtinPlugins.rabbitmq.manager.amqpUrl=amqps://fluidbg:fluidbg@rabbitmq.fluidbg-system:5671/%2f".to_string()
    } else {
        "builtinPlugins.rabbitmq.manager.amqpUrl=amqp://fluidbg:fluidbg@rabbitmq.fluidbg-system:5672/%2f".to_string()
    }
}

fn rabbitmq_manager_management_url(config: &E2eConfig) -> String {
    if config.full_tls {
        "builtinPlugins.rabbitmq.manager.managementUrl=https://rabbitmq.fluidbg-system:15671"
            .to_string()
    } else {
        "builtinPlugins.rabbitmq.manager.managementUrl=http://rabbitmq.fluidbg-system:15672"
            .to_string()
    }
}

fn write_tls_values_file() -> Result<std::path::PathBuf> {
    let path = std::env::temp_dir().join(format!("fluidbg-e2e-tls-{}.yaml", std::process::id()));
    let values = r#"
operator:
  api:
    tls:
      enabled: true
      certPath: /tls/tls.crt
      keyPath: /tls/tls.key
      caCertPath: /tls/ca.crt
  extraVolumes:
    - name: fluidbg-e2e-tls
      secret:
        secretName: fluidbg-e2e-tls
  extraVolumeMounts:
    - name: fluidbg-e2e-tls
      mountPath: /tls
      readOnly: true
builtinPlugins:
  http:
    controlPlaneTls:
      enabled: true
      certPath: /tls/tls.crt
      keyPath: /tls/tls.key
      caCertPath: /tls/ca.crt
      port: 9443
    inceptorVolumes:
      - name: fluidbg-e2e-tls
        secret:
          secretName: fluidbg-e2e-tls
    inceptorVolumeMounts:
      - name: fluidbg-e2e-tls
        mountPath: /tls
        readOnly: true
  rabbitmq:
    controlPlaneTls:
      enabled: true
      certPath: /tls/tls.crt
      keyPath: /tls/tls.key
      caCertPath: /tls/ca.crt
      port: 9090
    inceptorVolumes:
      - name: fluidbg-e2e-tls
        secret:
          secretName: fluidbg-e2e-tls
    inceptorVolumeMounts:
      - name: fluidbg-e2e-tls
        mountPath: /tls
        readOnly: true
    manager:
      amqpCaCertPath: /tls/ca.crt
      managementCaCertPath: /tls/ca.crt
      controlPlaneTls:
        enabled: true
        certPath: /tls/tls.crt
        keyPath: /tls/tls.key
        caCertPath: /tls/ca.crt
        port: 9090
      volumes:
        - name: fluidbg-e2e-tls
          secret:
            secretName: fluidbg-e2e-tls
      volumeMounts:
        - name: fluidbg-e2e-tls
          mountPath: /tls
          readOnly: true
"#;
    std::fs::write(&path, values).with_context(|| format!("write {}", path.display()))?;
    Ok(path)
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

    #[tokio::test]
    async fn wait_until_times_out_when_poll_future_hangs() {
        let started = Instant::now();
        let result = wait_until(Duration::from_millis(30), Duration::from_millis(5), || {
            std::future::pending::<bool>()
        })
        .await;

        assert!(result.is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn wait_for_value_times_out_when_poll_future_hangs() {
        let started = Instant::now();
        let result = wait_for_value(Duration::from_millis(30), Duration::from_millis(5), || {
            std::future::pending::<Option<String>>()
        })
        .await;

        assert!(result.is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
