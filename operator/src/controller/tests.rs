use super::{
    BlueGreenDeployment, ManagedDeploymentSpec, candidate_name_with_suffix,
    deterministic_candidate_suffixes, generated_candidate_name_seed,
    validate_progressive_splitter_plugin,
};
use crate::crd::blue_green::{BlueGreenDeploymentSpec, DeploymentSelector, PluginRef, TestSpec};
use crate::crd::inception_plugin::{
    InceptionPlugin, InceptionPluginSpec, PluginContainer, PluginFeatures, Topology,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use std::collections::BTreeMap;

fn sample_bgd(generation: i64) -> BlueGreenDeployment {
    BlueGreenDeployment {
        metadata: ObjectMeta {
            name: Some("order-processor-upgrade".to_string()),
            uid: Some("12345678-1234-5678-1234-567812345678".to_string()),
            generation: Some(generation),
            ..Default::default()
        },
        spec: BlueGreenDeploymentSpec {
            deployment: ManagedDeploymentSpec {
                namespace: Some("fluidbg-test".to_string()),
                spec: Default::default(),
            },
            selector: DeploymentSelector {
                namespace: None,
                match_labels: BTreeMap::new(),
            },
            inception_points: Vec::new(),
            tests: Vec::<TestSpec>::new(),
            state_store_ref: PluginRef {
                name: "memory-store".to_string(),
            },
            promotion: None,
        },
        status: None,
    }
}

fn sample_plugin(topology: Topology, supports_progressive_shifting: bool) -> InceptionPlugin {
    InceptionPlugin::new(
        "rabbitmq",
        InceptionPluginSpec {
            description: "test plugin".to_string(),
            image: "fluidbg/rabbitmq:dev".to_string(),
            supported_roles: Vec::new(),
            topology,
            field_namespaces: Vec::new(),
            config_schema: serde_json::json!({}),
            config_template: None,
            container: PluginContainer {
                ports: Vec::new(),
                volume_mounts: Vec::new(),
            },
            lifecycle: None,
            injects: None,
            features: Some(PluginFeatures {
                supports_progressive_shifting,
            }),
        },
    )
}

#[test]
fn deterministic_candidate_suffixes_are_stable_for_a_rollout() {
    let bgd = sample_bgd(3);
    let seed = generated_candidate_name_seed(&bgd);
    let first = deterministic_candidate_suffixes(&seed);
    let second = deterministic_candidate_suffixes(&seed);
    assert_eq!(first, second);
    assert_eq!(first.len(), 32);
    assert!(first.iter().all(|suffix| suffix.len() == 6));
}

#[test]
fn deterministic_candidate_suffixes_change_across_generations() {
    let first = deterministic_candidate_suffixes(&generated_candidate_name_seed(&sample_bgd(3)));
    let second = deterministic_candidate_suffixes(&generated_candidate_name_seed(&sample_bgd(4)));
    assert_ne!(first[0], second[0]);
}

#[test]
fn candidate_name_respects_dns_length_limit() {
    let base = "a".repeat(300);
    let candidate = candidate_name_with_suffix(&base, "abcdef");
    assert_eq!(candidate.len(), 253);
    assert!(candidate.ends_with("-abcdef"));
}

#[test]
fn progressive_splitter_requires_support_flag() {
    let plugin = sample_plugin(Topology::Standalone, false);
    let err =
        validate_progressive_splitter_plugin("incoming-orders", "rabbitmq", &plugin).unwrap_err();
    assert!(
        err.to_string()
            .contains("features.supportsProgressiveShifting=true")
    );
}

#[test]
fn progressive_splitter_requires_standalone_topology() {
    let plugin = sample_plugin(Topology::SidecarBlue, true);
    let err =
        validate_progressive_splitter_plugin("incoming-orders", "rabbitmq", &plugin).unwrap_err();
    assert!(err.to_string().contains("standalone splitter plugin"));
}

#[test]
fn progressive_splitter_accepts_standalone_plugin_with_support_flag() {
    let plugin = sample_plugin(Topology::Standalone, true);
    validate_progressive_splitter_plugin("incoming-orders", "rabbitmq", &plugin).unwrap();
}
