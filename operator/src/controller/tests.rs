use super::{
    BlueGreenDeployment, ManagedDeploymentSpec, candidate_name_with_suffix,
    deterministic_candidate_suffixes, generated_candidate_name_seed,
    select_previous_green_for_promotion, validate_progressive_splitter_plugin,
};
use crate::crd::blue_green::{
    BlueGreenDeploymentSpec, DeploymentRef, DeploymentSelector, PluginRef, TestSpec,
};
use crate::crd::inception_plugin::{
    InceptionPlugin, InceptionPluginSpec, PluginFeatures, PluginInceptor, Topology,
};
use k8s_openapi::api::apps::v1::Deployment;
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
            image: "fluidbg/fbg-plugin-rabbitmq:dev".to_string(),
            supported_roles: Vec::new(),
            topology,
            field_namespaces: Vec::new(),
            config_schema: serde_json::json!({}),
            config_template: None,
            inceptor: PluginInceptor {
                ports: Vec::new(),
                volume_mounts: Vec::new(),
                ..Default::default()
            },
            lifecycle: None,
            manager: None,
            injects: None,
            features: Some(PluginFeatures {
                supports_progressive_shifting,
            }),
        },
    )
}

fn deployment_named(name: &str) -> Deployment {
    Deployment {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            ..Default::default()
        },
        ..Default::default()
    }
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

#[test]
fn promotion_resume_treats_non_candidate_green_as_previous_green() {
    let candidate = DeploymentRef {
        name: "order-processor-upgrade-a1b2c3".to_string(),
        namespace: Some("fluidbg-test".to_string()),
    };
    let deployments = vec![
        deployment_named("order-processor-bootstrap"),
        deployment_named("order-processor-upgrade-a1b2c3"),
    ];

    let previous =
        select_previous_green_for_promotion("fluidbg-test", &deployments, &candidate, "default")
            .unwrap();

    assert_eq!(previous.name, "order-processor-bootstrap");
    assert_eq!(previous.namespace.as_deref(), Some("fluidbg-test"));
}

#[test]
fn promotion_resume_accepts_candidate_as_only_green() {
    let candidate = DeploymentRef {
        name: "order-processor-upgrade-a1b2c3".to_string(),
        namespace: Some("fluidbg-test".to_string()),
    };
    let deployments = vec![deployment_named("order-processor-upgrade-a1b2c3")];

    let previous =
        select_previous_green_for_promotion("fluidbg-test", &deployments, &candidate, "default")
            .unwrap();

    assert_eq!(previous.name, "order-processor-upgrade-a1b2c3");
    assert_eq!(previous.namespace.as_deref(), Some("fluidbg-test"));
}

#[test]
fn promotion_resume_rejects_multiple_previous_green_deployments() {
    let candidate = DeploymentRef {
        name: "order-processor-upgrade-a1b2c3".to_string(),
        namespace: Some("fluidbg-test".to_string()),
    };
    let deployments = vec![
        deployment_named("order-processor-bootstrap"),
        deployment_named("order-processor-v2"),
        deployment_named("order-processor-upgrade-a1b2c3"),
    ];

    let err =
        select_previous_green_for_promotion("fluidbg-test", &deployments, &candidate, "default")
            .unwrap_err();

    assert!(
        err.to_string()
            .contains("expected at most one previous green")
    );
}
