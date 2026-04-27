use std::collections::BTreeMap;

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum PluginRole {
    Duplicator,
    Splitter,
    Combiner,
    Observer,
    Mock,
    Writer,
    Consumer,
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
#[serde(rename_all = "camelCase")]
pub struct ContainerPort {
    pub name: String,
    pub container_port: i32,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VolumeMount {
    pub name: String,
    pub mount_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PluginContainer {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<ContainerPort>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volume_mounts: Vec<VolumeMount>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pod_labels: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pod_annotations: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_account_name: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvInjection {
    pub name_from_config: String,
    pub value_template: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restore_value_template: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct ContainerInjection {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<EnvInjection>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Injects {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub green_container: Option<ContainerInjection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blue_container: Option<ContainerInjection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_container: Option<ContainerInjection>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PluginFeatures {
    #[serde(default)]
    pub supports_progressive_shifting: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PluginLifecycle {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepare_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drain_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drain_status_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traffic_shift_path: Option<String>,
}

#[derive(Clone, Debug, CustomResource, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supported_roles: Vec<PluginRole>,
    pub topology: Topology,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub field_namespaces: Vec<String>,
    #[schemars(schema_with = "arbitrary_object_schema")]
    pub config_schema: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_template: Option<String>,
    pub container: PluginContainer,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<PluginLifecycle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub injects: Option<Injects>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub features: Option<PluginFeatures>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InceptionPluginStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
}

fn arbitrary_object_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "object",
        "x-kubernetes-preserve-unknown-fields": true
    })
}
