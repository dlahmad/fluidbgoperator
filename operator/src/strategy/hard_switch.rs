use crate::crd::blue_green::DataPromotionSpec;
use crate::state_store::Counts;
use crate::strategy::{PromotionAction, PromotionStrategy};

pub struct HardSwitchStrategy {
    pub min_test_cases: i64,
    pub success_rate: f64,
}

impl HardSwitchStrategy {
    pub fn from_criteria(criteria: &DataPromotionSpec) -> Self {
        Self {
            min_test_cases: criteria.min_test_cases.unwrap_or(100),
            success_rate: criteria.success_rate.unwrap_or(0.98),
        }
    }

    fn best_possible_rate(&self, counts: &Counts) -> Option<f64> {
        let best_total = counts.passed + counts.failed + counts.timed_out + counts.pending;
        if best_total < self.min_test_cases || best_total == 0 {
            return None;
        }
        Some((counts.passed + counts.pending) as f64 / best_total as f64)
    }
}

#[async_trait::async_trait]
impl PromotionStrategy for HardSwitchStrategy {
    async fn decide(&self, counts: &Counts, _current_step: Option<i32>) -> PromotionAction {
        if counts.pending > 0 {
            if let Some(best_possible_rate) = self.best_possible_rate(counts)
                && best_possible_rate < self.success_rate
            {
                return PromotionAction::Rollback;
            }
            return PromotionAction::ContinueObserving;
        }

        let total = counts.passed + counts.failed + counts.timed_out;
        if total < self.min_test_cases {
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
            min_test_cases: 100,
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
            min_test_cases: 100,
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
    async fn continue_observing_when_pending_cases_exist() {
        let strategy = HardSwitchStrategy {
            min_test_cases: 3,
            success_rate: 0.8,
        };
        let counts = Counts {
            passed: 4,
            failed: 0,
            timed_out: 0,
            pending: 1,
        };
        assert_eq!(
            strategy.decide(&counts, None).await,
            PromotionAction::ContinueObserving
        );
    }

    #[tokio::test]
    async fn rollback_when_pending_cases_cannot_recover_threshold() {
        let strategy = HardSwitchStrategy {
            min_test_cases: 3,
            success_rate: 0.8,
        };
        let counts = Counts {
            passed: 0,
            failed: 4,
            timed_out: 0,
            pending: 1,
        };
        assert_eq!(
            strategy.decide(&counts, None).await,
            PromotionAction::Rollback
        );
    }

    #[tokio::test]
    async fn rollback_when_rate_below_threshold() {
        let strategy = HardSwitchStrategy {
            min_test_cases: 100,
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
