use crate::crd::blue_green::{BlueGreenDeployment, CustomPromotionSpec, DataPromotionSpec};
use crate::crd::inception_plugin::PluginRole;
use crate::state_store::Counts;
use crate::strategy::hard_switch::HardSwitchStrategy;
use crate::strategy::progressive::ProgressiveStrategy;
use crate::strategy::{PromotionAction, PromotionStrategy};

use super::ReconcileError;

pub(super) async fn decide_promotion_action(
    bgd: &BlueGreenDeployment,
    data_counts: &Counts,
    custom_counts: &Counts,
    current_step: Option<i32>,
) -> std::result::Result<PromotionAction, ReconcileError> {
    let promotion = bgd.spec.promotion.as_ref().ok_or_else(|| {
        ReconcileError::Store("blue green deployment is missing promotion spec".into())
    })?;

    let data_action = if let Some(data) = promotion.data.as_ref() {
        Some(
            promotion_strategy_for(bgd, data)?
                .decide(data_counts, current_step)
                .await,
        )
    } else {
        None
    };

    let custom_action = promotion
        .custom
        .as_ref()
        .map(|custom| decide_custom_promotion(custom_counts, custom));

    match (data_action, custom_action) {
        (Some(PromotionAction::Rollback), _) | (_, Some(PromotionAction::Rollback)) => {
            Ok(PromotionAction::Rollback)
        }
        (Some(PromotionAction::ContinueObserving), _)
        | (_, Some(PromotionAction::ContinueObserving)) => Ok(PromotionAction::ContinueObserving),
        (
            Some(PromotionAction::AdvanceStep {
                step,
                traffic_percent,
            }),
            None,
        ) => Ok(PromotionAction::AdvanceStep {
            step,
            traffic_percent,
        }),
        (Some(PromotionAction::Promote), Some(PromotionAction::Promote))
        | (Some(PromotionAction::Promote), None)
        | (None, Some(PromotionAction::Promote)) => Ok(PromotionAction::Promote),
        (None, None) => Err(ReconcileError::Store(
            "promotion must define at least one of data or custom".to_string(),
        )),
        _ => Ok(PromotionAction::ContinueObserving),
    }
}

fn decide_custom_promotion(counts: &Counts, _custom: &CustomPromotionSpec) -> PromotionAction {
    if counts.pending > 0 {
        return PromotionAction::ContinueObserving;
    }
    if counts.failed > 0 || counts.timed_out > 0 {
        return PromotionAction::Rollback;
    }
    if counts.passed == 0 {
        return PromotionAction::ContinueObserving;
    }
    PromotionAction::Promote
}

fn promotion_strategy_for(
    bgd: &BlueGreenDeployment,
    data_promotion: &DataPromotionSpec,
) -> std::result::Result<Box<dyn PromotionStrategy>, ReconcileError> {
    let promotion = bgd.spec.promotion.as_ref().ok_or_else(|| {
        ReconcileError::Store("blue green deployment is missing promotion spec".into())
    })?;

    match promotion.strategy.strategy_type {
        crate::crd::blue_green::StrategyType::HardSwitch => {
            Ok(Box::new(HardSwitchStrategy::from_criteria(data_promotion)))
        }
        crate::crd::blue_green::StrategyType::Progressive => {
            let progressive = promotion.strategy.progressive.as_ref().ok_or_else(|| {
                ReconcileError::Store("progressive strategy selected without steps".into())
            })?;
            Ok(Box::new(ProgressiveStrategy::from_steps(
                progressive.steps.clone(),
                progressive.rollback_on_step_failure,
            )))
        }
    }
}

pub(super) fn validate_test_configuration(
    bgd: &BlueGreenDeployment,
) -> std::result::Result<(), ReconcileError> {
    let Some(promotion) = bgd.spec.promotion.as_ref() else {
        if bgd.spec.tests.is_empty() {
            return Ok(());
        }
        return Err(ReconcileError::Store(
            "tests require a promotion spec".to_string(),
        ));
    };

    if promotion.data.is_none() && promotion.custom.is_none() {
        return Err(ReconcileError::Store(
            "promotion must define at least one of data or custom".to_string(),
        ));
    }

    for test in &bgd.spec.tests {
        if test.data_verification.is_none() && test.custom_verification.is_none() {
            return Err(ReconcileError::Store(format!(
                "test '{}' must define at least one of dataVerification or customVerification",
                test.name
            )));
        }
        if test.data_verification.is_some() && promotion.data.is_none() {
            return Err(ReconcileError::Store(format!(
                "test '{}' uses dataVerification but promotion.data is missing",
                test.name
            )));
        }
        if test.custom_verification.is_some() && promotion.custom.is_none() {
            return Err(ReconcileError::Store(format!(
                "test '{}' uses customVerification but promotion.custom is missing",
                test.name
            )));
        }
    }

    Ok(())
}

pub(super) fn initial_splitter_traffic_percent(bgd: &BlueGreenDeployment) -> Option<i32> {
    if !has_splitter(bgd) {
        return None;
    }
    match bgd.spec.promotion.as_ref()?.strategy.strategy_type {
        crate::crd::blue_green::StrategyType::HardSwitch => Some(100),
        crate::crd::blue_green::StrategyType::Progressive => bgd
            .spec
            .promotion
            .as_ref()?
            .strategy
            .progressive
            .as_ref()?
            .steps
            .first()
            .map(|step| step.traffic_percent),
    }
}

fn has_splitter(bgd: &BlueGreenDeployment) -> bool {
    bgd.spec.inception_points.iter().any(|ip| {
        ip.roles
            .iter()
            .any(|role| matches!(role, PluginRole::Splitter))
    })
}

#[cfg(test)]
mod tests {
    use super::{decide_promotion_action, validate_test_configuration};
    use crate::crd::blue_green::{
        BlueGreenDeployment, BlueGreenDeploymentSpec, CustomPromotionSpec, CustomVerificationSpec,
        DataPromotionSpec, DataVerificationSpec, DeploymentSelector, ManagedDeploymentSpec,
        PluginRef, PromotionSpec, PromotionStrategy, StrategyType, TestSpec,
    };
    use crate::state_store::Counts;
    use crate::strategy::PromotionAction;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
    use std::collections::BTreeMap;

    fn base_bgd() -> BlueGreenDeployment {
        BlueGreenDeployment {
            metadata: ObjectMeta {
                name: Some("orders".to_string()),
                ..Default::default()
            },
            spec: BlueGreenDeploymentSpec {
                state_store_ref: PluginRef {
                    name: "memory".to_string(),
                },
                selector: DeploymentSelector {
                    namespace: None,
                    match_labels: BTreeMap::from([("app".to_string(), "orders".to_string())]),
                },
                deployment: ManagedDeploymentSpec {
                    namespace: None,
                    spec: Default::default(),
                },
                inception_points: Vec::new(),
                tests: Vec::new(),
                promotion: Some(PromotionSpec {
                    data: None,
                    custom: Some(CustomPromotionSpec {
                        timeout_seconds: None,
                        retries: None,
                    }),
                    strategy: PromotionStrategy {
                        strategy_type: StrategyType::HardSwitch,
                        progressive: None,
                    },
                }),
            },
            status: None,
        }
    }

    #[tokio::test]
    async fn custom_promotion_waits_for_pending_cases() {
        let action = decide_promotion_action(
            &base_bgd(),
            &Counts::default(),
            &Counts {
                pending: 1,
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();

        assert_eq!(action, PromotionAction::ContinueObserving);
    }

    #[tokio::test]
    async fn custom_promotion_rolls_back_on_failure_or_timeout() {
        let bgd = base_bgd();
        let failed = decide_promotion_action(
            &bgd,
            &Counts::default(),
            &Counts {
                failed: 1,
                passed: 3,
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
        let timed_out = decide_promotion_action(
            &bgd,
            &Counts::default(),
            &Counts {
                timed_out: 1,
                passed: 3,
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();

        assert_eq!(failed, PromotionAction::Rollback);
        assert_eq!(timed_out, PromotionAction::Rollback);
    }

    #[tokio::test]
    async fn custom_promotion_promotes_only_after_a_pass() {
        let action = decide_promotion_action(
            &base_bgd(),
            &Counts::default(),
            &Counts {
                passed: 1,
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();

        assert_eq!(action, PromotionAction::Promote);
    }

    #[test]
    fn validation_rejects_data_test_without_data_promotion() {
        let mut bgd = base_bgd();
        bgd.spec.tests.push(TestSpec {
            name: "verifier".to_string(),
            image: "verifier:dev".to_string(),
            port: 8080,
            data_verification: Some(DataVerificationSpec {
                verify_path: "/result/{testId}".to_string(),
                timeout_seconds: None,
            }),
            custom_verification: None,
            env: Vec::new(),
        });

        let err = validate_test_configuration(&bgd).unwrap_err().to_string();
        assert!(err.contains("promotion.data is missing"));
    }

    #[test]
    fn validation_accepts_matching_data_and_custom_modes() {
        let mut bgd = base_bgd();
        bgd.spec.promotion = Some(PromotionSpec {
            data: Some(DataPromotionSpec {
                min_test_cases: Some(1),
                success_rate: Some(1.0),
                timeout_seconds: None,
            }),
            custom: Some(CustomPromotionSpec {
                timeout_seconds: None,
                retries: Some(2),
            }),
            strategy: PromotionStrategy {
                strategy_type: StrategyType::HardSwitch,
                progressive: None,
            },
        });
        bgd.spec.tests.push(TestSpec {
            name: "verifier".to_string(),
            image: "verifier:dev".to_string(),
            port: 8080,
            data_verification: Some(DataVerificationSpec {
                verify_path: "/result/{testId}".to_string(),
                timeout_seconds: None,
            }),
            custom_verification: Some(CustomVerificationSpec {
                start_path: "/start".to_string(),
                verify_path: "/result/{testId}".to_string(),
                timeout_seconds: None,
                retries: Some(2),
            }),
            env: Vec::new(),
        });

        validate_test_configuration(&bgd).unwrap();
    }
}
