//! Non-bypassable hard safety cage policy for autonomous and self-modifying strategies.

use std::collections::{HashMap, HashSet};

use exchange_core::AssetClass;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardSafetyPolicy {
    pub max_total_notional_cents: i64,
    pub max_strategy_canary_notional_cents: i64,
    pub max_orders_per_minute: u32,
    pub max_drawdown_cents: i64,
    pub forced_cooldown_secs: u64,
    pub max_venue_notional_cents: i64,
    pub max_asset_class_notional_cents: i64,
}

impl Default for HardSafetyPolicy {
    fn default() -> Self {
        Self {
            max_total_notional_cents: 50_000,
            max_strategy_canary_notional_cents: 2_500,
            max_orders_per_minute: 120,
            max_drawdown_cents: 5_000,
            forced_cooldown_secs: 600,
            max_venue_notional_cents: 25_000,
            max_asset_class_notional_cents: 35_000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RiskSnapshot {
    pub total_notional_cents: i64,
    pub drawdown_cents: i64,
    pub orders_last_minute: u32,
    pub kill_switch_engaged: bool,
    pub paused: bool,
    pub strategy_canary_notional: HashMap<String, i64>,
    pub venue_notional: HashMap<String, i64>,
    pub asset_class_notional: HashMap<AssetClass, i64>,
    pub scoped_kill_venues: HashSet<String>,
    pub scoped_kill_strategies: HashSet<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromotionRequest {
    pub strategy_id: String,
    pub code_hash: String,
    pub requested_canary_notional_cents: i64,
    pub compile_passed: bool,
    pub replay_passed: bool,
    pub paper_passed: bool,
    pub latency_passed: bool,
    pub risk_passed: bool,
}

impl PromotionRequest {
    pub fn all_gates_passed(&self) -> bool {
        self.compile_passed
            && self.replay_passed
            && self.paper_passed
            && self.latency_passed
            && self.risk_passed
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum RiskDecision {
    Allow,
    Deny { reason: String },
}

#[derive(Debug, Clone)]
pub struct HardSafetyCage {
    policy: HardSafetyPolicy,
}

impl HardSafetyCage {
    pub fn new(policy: HardSafetyPolicy) -> Self {
        Self { policy }
    }

    pub fn policy(&self) -> &HardSafetyPolicy {
        &self.policy
    }

    pub fn evaluate_order(
        &self,
        strategy_id: &str,
        requested_notional_cents: i64,
        snapshot: &RiskSnapshot,
    ) -> RiskDecision {
        self.evaluate_order_with_scope(
            strategy_id,
            "unknown",
            &AssetClass::Custom,
            requested_notional_cents,
            snapshot,
        )
    }

    pub fn evaluate_order_with_scope(
        &self,
        strategy_id: &str,
        venue: &str,
        asset_class: &AssetClass,
        requested_notional_cents: i64,
        snapshot: &RiskSnapshot,
    ) -> RiskDecision {
        if snapshot.kill_switch_engaged {
            return RiskDecision::Deny {
                reason: "kill switch engaged".to_string(),
            };
        }
        if snapshot.paused {
            return RiskDecision::Deny {
                reason: "engine paused".to_string(),
            };
        }
        if snapshot.scoped_kill_venues.contains(venue) {
            return RiskDecision::Deny {
                reason: format!("venue '{}' is kill-switched", venue),
            };
        }
        if snapshot.scoped_kill_strategies.contains(strategy_id) {
            return RiskDecision::Deny {
                reason: format!("strategy '{}' is kill-switched", strategy_id),
            };
        }
        if requested_notional_cents <= 0 {
            return RiskDecision::Deny {
                reason: "requested notional must be positive".to_string(),
            };
        }
        if snapshot.orders_last_minute >= self.policy.max_orders_per_minute {
            return RiskDecision::Deny {
                reason: format!(
                    "orders per minute limit breached: {} >= {}",
                    snapshot.orders_last_minute, self.policy.max_orders_per_minute
                ),
            };
        }
        if snapshot.drawdown_cents > self.policy.max_drawdown_cents {
            return RiskDecision::Deny {
                reason: format!(
                    "drawdown breached: {} > {}",
                    snapshot.drawdown_cents, self.policy.max_drawdown_cents
                ),
            };
        }

        let projected_total = snapshot
            .total_notional_cents
            .saturating_add(requested_notional_cents);
        if projected_total > self.policy.max_total_notional_cents {
            return RiskDecision::Deny {
                reason: format!(
                    "total notional breached: {} > {}",
                    projected_total, self.policy.max_total_notional_cents
                ),
            };
        }

        let current_strategy = snapshot
            .strategy_canary_notional
            .get(strategy_id)
            .copied()
            .unwrap_or(0);
        let projected_strategy = current_strategy.saturating_add(requested_notional_cents);
        if projected_strategy > self.policy.max_strategy_canary_notional_cents {
            return RiskDecision::Deny {
                reason: format!(
                    "strategy canary notional breached: {} > {}",
                    projected_strategy, self.policy.max_strategy_canary_notional_cents
                ),
            };
        }

        let current_venue = snapshot.venue_notional.get(venue).copied().unwrap_or(0);
        let projected_venue = current_venue.saturating_add(requested_notional_cents);
        if projected_venue > self.policy.max_venue_notional_cents {
            return RiskDecision::Deny {
                reason: format!(
                    "venue notional breached for {}: {} > {}",
                    venue, projected_venue, self.policy.max_venue_notional_cents
                ),
            };
        }

        let current_asset = snapshot
            .asset_class_notional
            .get(asset_class)
            .copied()
            .unwrap_or(0);
        let projected_asset = current_asset.saturating_add(requested_notional_cents);
        if projected_asset > self.policy.max_asset_class_notional_cents {
            return RiskDecision::Deny {
                reason: format!(
                    "asset class notional breached for {:?}: {} > {}",
                    asset_class, projected_asset, self.policy.max_asset_class_notional_cents
                ),
            };
        }

        RiskDecision::Allow
    }

    pub fn evaluate_promotion(
        &self,
        request: &PromotionRequest,
        snapshot: &RiskSnapshot,
    ) -> RiskDecision {
        if snapshot.kill_switch_engaged {
            return RiskDecision::Deny {
                reason: "kill switch engaged".to_string(),
            };
        }
        if snapshot.paused {
            return RiskDecision::Deny {
                reason: "engine paused".to_string(),
            };
        }
        if snapshot
            .scoped_kill_strategies
            .contains(&request.strategy_id)
        {
            return RiskDecision::Deny {
                reason: format!("strategy '{}' is kill-switched", request.strategy_id),
            };
        }
        if !request.all_gates_passed() {
            return RiskDecision::Deny {
                reason: "promotion gates did not pass".to_string(),
            };
        }
        if request.requested_canary_notional_cents <= 0 {
            return RiskDecision::Deny {
                reason: "requested canary notional must be positive".to_string(),
            };
        }

        let projected_strategy = request.requested_canary_notional_cents;

        if projected_strategy > self.policy.max_strategy_canary_notional_cents {
            return RiskDecision::Deny {
                reason: format!(
                    "strategy canary notional breached: {} > {}",
                    projected_strategy, self.policy.max_strategy_canary_notional_cents
                ),
            };
        }

        RiskDecision::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denies_when_kill_switch_engaged() {
        let cage = HardSafetyCage::new(HardSafetyPolicy::default());
        let snapshot = RiskSnapshot {
            kill_switch_engaged: true,
            ..RiskSnapshot::default()
        };
        let result = cage.evaluate_order_with_scope(
            "strategy.a",
            "coinbase_at",
            &AssetClass::Crypto,
            100,
            &snapshot,
        );
        assert_eq!(
            result,
            RiskDecision::Deny {
                reason: "kill switch engaged".to_string()
            }
        );
    }

    #[test]
    fn denies_when_venue_is_killed() {
        let cage = HardSafetyCage::new(HardSafetyPolicy::default());
        let mut snapshot = RiskSnapshot::default();
        snapshot
            .scoped_kill_venues
            .insert("coinbase_at".to_string());

        let result = cage.evaluate_order_with_scope(
            "strategy.a",
            "coinbase_at",
            &AssetClass::Crypto,
            100,
            &snapshot,
        );
        assert_eq!(
            result,
            RiskDecision::Deny {
                reason: "venue 'coinbase_at' is kill-switched".to_string()
            }
        );
    }

    #[test]
    fn denies_promotion_when_any_gate_fails() {
        let cage = HardSafetyCage::new(HardSafetyPolicy::default());
        let snapshot = RiskSnapshot::default();
        let req = PromotionRequest {
            strategy_id: "strategy.a".to_string(),
            code_hash: "hash".to_string(),
            requested_canary_notional_cents: 100,
            compile_passed: true,
            replay_passed: false,
            paper_passed: true,
            latency_passed: true,
            risk_passed: true,
        };

        let result = cage.evaluate_promotion(&req, &snapshot);
        assert_eq!(
            result,
            RiskDecision::Deny {
                reason: "promotion gates did not pass".to_string()
            }
        );
    }

    #[test]
    fn allows_promotion_that_replaces_usage() {
        let mut policy = HardSafetyPolicy::default();
        policy.max_strategy_canary_notional_cents = 1000;
        let cage = HardSafetyCage::new(policy);

        let mut snapshot = RiskSnapshot::default();
        snapshot
            .strategy_canary_notional
            .insert("strategy.a".to_string(), 800);

        let req = PromotionRequest {
            strategy_id: "strategy.a".to_string(),
            code_hash: "hash".to_string(),
            requested_canary_notional_cents: 900,
            compile_passed: true,
            replay_passed: true,
            paper_passed: true,
            latency_passed: true,
            risk_passed: true,
        };

        let result = cage.evaluate_promotion(&req, &snapshot);
        assert_eq!(result, RiskDecision::Allow);
    }
}
