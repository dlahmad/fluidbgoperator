pub mod hard_switch;
pub mod progressive;

use async_trait::async_trait;

#[derive(Debug, Clone, PartialEq)]
pub enum PromotionAction {
    ContinueObserving,
    Promote,
    Rollback,
    AdvanceStep { step: i32, traffic_percent: i32 },
}

#[async_trait]
pub trait PromotionStrategy: Send + Sync {
    async fn decide(
        &self,
        counts: &crate::state_store::Counts,
        current_step: Option<i32>,
    ) -> PromotionAction;
}
