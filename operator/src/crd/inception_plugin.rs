use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, JsonSchema)]
pub enum PluginMode {
    #[serde(rename = "trigger")]
    Trigger,
    #[serde(rename = "passthrough-duplicate")]
    PassthroughDuplicate,
    #[serde(rename = "reroute-mock")]
    RerouteMock,
    #[serde(rename = "write")]
    Write,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, JsonSchema)]
pub enum Direction {
    #[serde(rename = "ingress")]
    Ingress,
    #[serde(rename = "egress")]
    Egress,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub enum Topology {
    #[serde(rename = "sidecar-blue")]
    SidecarBlue,
    #[serde(rename = "sidecar-test")]
    SidecarTest,
    #[serde(rename = "standalone")]
    Standalone,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct ContainerPort {
    pub name: String,
    pub container_port: i32,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct VolumeMount {
    pub name: String,
    pub mount_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct PluginContainer {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<ContainerPort>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volume_mounts: Vec<VolumeMount>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct EnvInjection {
    pub name_from_config: String,
    pub value_template: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct ContainerInjection {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<EnvInjection>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct Injects {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blue_container: Option<ContainerInjection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_container: Option<ContainerInjection>,
}

#[derive(Clone, Debug, CustomResource, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "fluidbg.io",
    version = "v1alpha1",
    kind = "InceptionPlugin",
    namespaced
)]
#[kube(status = "InceptionPluginStatus")]
pub struct InceptionPluginSpec {
    pub description: String,
    pub image: String,
    pub topology: Topology,
    pub modes: Vec<PluginMode>,
    pub directions: Vec<Direction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub field_namespaces: Vec<String>,
    pub config_schema: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_template: Option<String>,
    pub container: PluginContainer,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub injects: Option<Injects>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
pub struct InceptionPluginStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
}
