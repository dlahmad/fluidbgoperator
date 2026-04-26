use super::{
    BlueGreenDeployment, ManagedDeploymentSpec, candidate_name_with_suffix,
    deterministic_candidate_suffixes, generated_candidate_name_seed,
};
use crate::crd::blue_green::{BlueGreenDeploymentSpec, DeploymentSelector, PluginRef, TestSpec};
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
