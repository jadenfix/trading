use crate::types::{Decision, DeterministicInput, TradeIntent, VetoReason};
use common::{Action, FeeSchedule};
use llm_client::{ResearchResponse, RiskLevel};

pub struct DecisionEngine {
    fee_schedule: FeeSchedule,
    min_edge_cents: i64,
    max_position_size: i64,
    live_min_size_contracts: i64,
    kelly_fraction: f64,
    max_uncertainty: f64,
    rules_risk_veto_level: RiskLevel,
    uncertainty_penalty_cents_mult: f64,
}

impl DecisionEngine {
    pub fn new(
        min_edge_cents: i64,
        max_position_size: i64,
        live_min_size_contracts: i64,
        kelly_fraction: f64,
        max_uncertainty: f64,
        rules_risk_veto_level: RiskLevel,
    ) -> Self {
        Self {
            fee_schedule: FeeSchedule::default(),
            min_edge_cents,
            max_position_size,
            live_min_size_contracts,
            kelly_fraction,
            max_uncertainty,
            rules_risk_veto_level,
            uncertainty_penalty_cents_mult: 5.0,
        }
    }

    fn risk_level_rank(level: &RiskLevel) -> u8 {
        match level {
            RiskLevel::Low => 0,
            RiskLevel::Medium => 1,
            RiskLevel::High => 2,
            RiskLevel::Critical => 3,
        }
    }

    fn size_from_kelly(&self, p_hat: f64, price_cents: i64) -> i64 {
        let p_eff = (price_cents as f64 / 100.0).clamp(0.01, 0.99);
        let denom = 1.0 - p_eff;
        if denom <= 0.0 {
            return 0;
        }

        let f_star = ((p_hat - p_eff) / denom).max(0.0);
        let f = (self.kelly_fraction * f_star).clamp(0.0, 1.0);
        let size = (f * self.max_position_size as f64).ceil() as i64;
        size.clamp(0, self.max_position_size)
    }

    fn deterministic_edge_after_fees_cents(&self, input: &DeterministicInput) -> f64 {
        let fee_cents = self.fee_schedule.per_contract_taker_fee(input.target_price_cents);
        (input.p_det_target * 100.0) - input.target_price_cents as f64 - fee_cents
    }

    pub fn evaluate_with_research(
        &self,
        input: &DeterministicInput,
        research: &ResearchResponse,
    ) -> Decision {
        if research.uncertainty > self.max_uncertainty {
            return Decision::Veto(VetoReason {
                ticker: input.ticker.clone(),
                reason: format!(
                    "High uncertainty: {:.3} > {:.3}",
                    research.uncertainty, self.max_uncertainty
                ),
            });
        }

        if Self::risk_level_rank(&research.risk_of_misresolution)
            >= Self::risk_level_rank(&self.rules_risk_veto_level)
        {
            return Decision::Veto(VetoReason {
                ticker: input.ticker.clone(),
                reason: format!(
                    "Rules-risk veto: {:?} >= {:?}",
                    research.risk_of_misresolution, self.rules_risk_veto_level
                ),
            });
        }

        let base_edge_cents = self.deterministic_edge_after_fees_cents(input);
        let risk_penalty_cents = match research.risk_of_misresolution {
            RiskLevel::Low => 0.0,
            RiskLevel::Medium => 1.0,
            RiskLevel::High => 3.0,
            RiskLevel::Critical => 5.0,
        };
        let uncertainty_penalty_cents = research.uncertainty * self.uncertainty_penalty_cents_mult;
        let edge_penalized_cents = base_edge_cents - risk_penalty_cents - uncertainty_penalty_cents;

        if edge_penalized_cents < self.min_edge_cents as f64 {
            return Decision::Veto(VetoReason {
                ticker: input.ticker.clone(),
                reason: format!(
                    "Edge below threshold after penalties: {:.2} < {}",
                    edge_penalized_cents, self.min_edge_cents
                ),
            });
        }

        let size = self.size_from_kelly(input.p_det_target, input.target_price_cents);
        if size < self.live_min_size_contracts {
            return Decision::Veto(VetoReason {
                ticker: input.ticker.clone(),
                reason: format!(
                    "Size below live minimum: {} < {}",
                    size, self.live_min_size_contracts
                ),
            });
        }

        Decision::Approve(TradeIntent {
            ticker: input.ticker.clone(),
            side: input.target_side,
            action: Action::Buy,
            price_cents: input.target_price_cents,
            size_contracts: size,
            edge_cents: edge_penalized_cents,
            confidence: research.confidence,
            reasons: vec![format!(
                "edge_penalized={:.2} base={:.2} uncertainty_penalty={:.2} risk_penalty={:.2}",
                edge_penalized_cents, base_edge_cents, uncertainty_penalty_cents, risk_penalty_cents
            )],
        })
    }

    pub fn evaluate_deterministic(&self, input: &DeterministicInput, reason: &str) -> Decision {
        let edge_cents = self.deterministic_edge_after_fees_cents(input);
        if edge_cents < self.min_edge_cents as f64 {
            return Decision::Veto(VetoReason {
                ticker: input.ticker.clone(),
                reason: format!(
                    "Deterministic fallback edge below threshold: {:.2} < {} ({})",
                    edge_cents, self.min_edge_cents, reason
                ),
            });
        }

        let size = self.size_from_kelly(input.p_det_target, input.target_price_cents);
        if size < self.live_min_size_contracts {
            return Decision::Veto(VetoReason {
                ticker: input.ticker.clone(),
                reason: format!(
                    "Deterministic fallback size below minimum: {} < {}",
                    size, self.live_min_size_contracts
                ),
            });
        }

        Decision::Approve(TradeIntent {
            ticker: input.ticker.clone(),
            side: input.target_side,
            action: Action::Buy,
            price_cents: input.target_price_cents,
            size_contracts: size,
            edge_cents,
            confidence: 0.5,
            reasons: vec![format!("deterministic_fallback: {}", reason)],
        })
    }
}
