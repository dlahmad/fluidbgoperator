use crate::crd::blue_green::SuccessCriteria;
use crate::state_store::Counts;
use crate::strategy::{PromotionAction, PromotionStrategy};

pub struct HardSwitchStrategy {
    pub min_cases: i64,
    pub success_rate: f64,
}

impl HardSwitchStrategy {
    pub fn from_criteria(criteria: &SuccessCriteria) -> Self {
        Self {
            min_cases: criteria.min_cases.unwrap_or(100),
            success_rate: criteria.success_rate.unwrap_or(0.98),
        }
    }
}

#[async_trait::async_trait]
impl PromotionStrategy for HardSwitchStrategy {
    async fn decide(&self, counts: &Counts, _current_step: Option<i32>) -> PromotionAction {
        let total = counts.passed + counts.failed + counts.timed_out;
        if total < self.min_cases {
            return PromotionAction::ContinueObserving;
        }

        let actual_rate = if total > 0 {
            counts.passed as f64 / total as f64
        } else {
            0.0
        };

        if actual_rate >= self.success_rate {
            PromotionAction::Promote
        } else {
            PromotionAction::Rollback
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state_store::Counts;

    #[tokio::test]
    async fn continue_observing_when_insufficient_cases() {
        let strategy = HardSwitchStrategy {
            min_cases: 100,
            success_rate: 0.98,
        };
        let counts = Counts {
            passed: 50,
            failed: 0,
            timed_out: 0,
            pending: 10,
        };
        assert_eq!(
            strategy.decide(&counts, None).await,
            PromotionAction::ContinueObserving
        );
    }

    #[tokio::test]
    async fn promote_when_rate_meets_threshold() {
        let strategy = HardSwitchStrategy {
            min_cases: 100,
            success_rate: 0.98,
        };
        let counts = Counts {
            passed: 100,
            failed: 0,
            timed_out: 0,
            pending: 0,
        };
        assert_eq!(
            strategy.decide(&counts, None).await,
            PromotionAction::Promote
        );
    }

    #[tokio::test]
    async fn rollback_when_rate_below_threshold() {
        let strategy = HardSwitchStrategy {
            min_cases: 100,
            success_rate: 0.98,
        };
        let counts = Counts {
            passed: 90,
            failed: 10,
            timed_out: 0,
            pending: 0,
        };
        assert_eq!(
            strategy.decide(&counts, None).await,
            PromotionAction::Rollback
        );
    }
}
