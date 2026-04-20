use crate::crd::blue_green::ProgressiveStep;
use crate::state_store::Counts;
use crate::strategy::{PromotionAction, PromotionStrategy};

pub struct ProgressiveStrategy {
    pub steps: Vec<ProgressiveStep>,
    pub rollback_on_step_failure: bool,
}

impl ProgressiveStrategy {
    pub fn from_steps(steps: Vec<ProgressiveStep>, rollback_on_step_failure: bool) -> Self {
        Self {
            steps,
            rollback_on_step_failure,
        }
    }
}

#[async_trait::async_trait]
impl PromotionStrategy for ProgressiveStrategy {
    async fn decide(&self, counts: &Counts, current_step: Option<i32>) -> PromotionAction {
        let step_idx = current_step.unwrap_or(0) as usize;

        if step_idx >= self.steps.len() {
            return PromotionAction::Promote;
        }

        let step = &self.steps[step_idx];
        let total = counts.passed + counts.failed + counts.timed_out;

        if total < step.observe_cases {
            return PromotionAction::ContinueObserving;
        }

        let actual_rate = if total > 0 {
            counts.passed as f64 / total as f64
        } else {
            0.0
        };

        if actual_rate < step.success_rate {
            if self.rollback_on_step_failure {
                return PromotionAction::Rollback;
            }
            return PromotionAction::ContinueObserving;
        }

        if step_idx + 1 >= self.steps.len() {
            return PromotionAction::Promote;
        }

        let next_step = &self.steps[step_idx + 1];
        PromotionAction::AdvanceStep {
            step: (step_idx + 1) as i32,
            traffic_percent: next_step.traffic_percent,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crd::blue_green::ProgressiveStep;

    fn make_steps() -> Vec<ProgressiveStep> {
        vec![
            ProgressiveStep {
                traffic_percent: 5,
                observe_cases: 20,
                success_rate: 0.99,
            },
            ProgressiveStep {
                traffic_percent: 25,
                observe_cases: 50,
                success_rate: 0.98,
            },
            ProgressiveStep {
                traffic_percent: 100,
                observe_cases: 100,
                success_rate: 0.98,
            },
        ]
    }

    #[tokio::test]
    async fn continue_observing_when_insufficient_cases() {
        let strategy = ProgressiveStrategy::from_steps(make_steps(), true);
        let counts = Counts {
            passed: 10,
            failed: 0,
            timed_out: 0,
            pending: 5,
        };
        assert_eq!(
            strategy.decide(&counts, Some(0)).await,
            PromotionAction::ContinueObserving
        );
    }

    #[tokio::test]
    async fn advance_to_next_step() {
        let strategy = ProgressiveStrategy::from_steps(make_steps(), true);
        let counts = Counts {
            passed: 20,
            failed: 0,
            timed_out: 0,
            pending: 0,
        };
        assert_eq!(
            strategy.decide(&counts, Some(0)).await,
            PromotionAction::AdvanceStep {
                step: 1,
                traffic_percent: 25
            }
        );
    }

    #[tokio::test]
    async fn rollback_on_step_failure() {
        let strategy = ProgressiveStrategy::from_steps(make_steps(), true);
        let counts = Counts {
            passed: 15,
            failed: 5,
            timed_out: 0,
            pending: 0,
        };
        assert_eq!(
            strategy.decide(&counts, Some(0)).await,
            PromotionAction::Rollback
        );
    }

    #[tokio::test]
    async fn promote_at_last_step() {
        let strategy = ProgressiveStrategy::from_steps(make_steps(), true);
        let counts = Counts {
            passed: 100,
            failed: 0,
            timed_out: 0,
            pending: 0,
        };
        assert_eq!(
            strategy.decide(&counts, Some(2)).await,
            PromotionAction::Promote
        );
    }

    #[tokio::test]
    async fn no_rollback_when_flag_disabled() {
        let strategy = ProgressiveStrategy::from_steps(make_steps(), false);
        let counts = Counts {
            passed: 15,
            failed: 5,
            timed_out: 0,
            pending: 0,
        };
        assert_eq!(
            strategy.decide(&counts, Some(0)).await,
            PromotionAction::ContinueObserving
        );
    }
}
