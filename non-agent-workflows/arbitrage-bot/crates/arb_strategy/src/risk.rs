//! Risk manager — enforces exposure limits and anti-spam gates.

use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use common::Error;
use tracing::{info, warn};

use crate::arb::ArbOpportunity;
use crate::config::ArbRiskConfig;

pub struct ArbRiskManager {
    config: ArbRiskConfig,
    /// Track exposure per event ticker (cents).
    event_exposure: HashMap<String, i64>,
    /// Track total portfolio exposure (cents).
    total_exposure: i64,
    /// Attempt timestamps per group (for rate limiting).
    attempt_history: HashMap<String, VecDeque<Instant>>,
    /// Submitted order timestamps (rolling 60s window).
    order_history: VecDeque<Instant>,
    /// Balance at bot startup (for drawdown checks).
    starting_balance_cents: Option<i64>,
    /// Latest observed balance.
    latest_balance_cents: Option<i64>,
    /// Consecutive critical execution failures.
    consecutive_critical_failures: u32,
    /// Kill switch state.
    kill_switch_engaged: bool,
}

impl ArbRiskManager {
    pub fn new(config: ArbRiskConfig) -> Self {
        Self {
            config,
            event_exposure: HashMap::new(),
            total_exposure: 0,
            attempt_history: HashMap::new(),
            order_history: VecDeque::new(),
            starting_balance_cents: None,
            latest_balance_cents: None,
            consecutive_critical_failures: 0,
            kill_switch_engaged: false,
        }
    }

    fn prune_rolling_window(history: &mut VecDeque<Instant>, now: Instant) {
        while history
            .front()
            .is_some_and(|t| now.duration_since(*t).as_secs() >= 60)
        {
            history.pop_front();
        }
    }

    fn check_balance_limits(&self) -> Result<(), Error> {
        let balance = self.latest_balance_cents.ok_or_else(|| {
            Error::RiskViolation(
                "Account balance has not been initialized; refusing live execution".into(),
            )
        })?;

        if balance < self.config.min_balance_cents {
            return Err(Error::RiskViolation(format!(
                "Balance below minimum: {} < {}",
                balance, self.config.min_balance_cents
            )));
        }

        if let Some(starting_balance) = self.starting_balance_cents {
            let drawdown = (starting_balance - balance).max(0);
            if drawdown > self.config.max_daily_loss_cents {
                return Err(Error::RiskViolation(format!(
                    "Daily drawdown limit breached: {} > {}",
                    drawdown, self.config.max_daily_loss_cents
                )));
            }
        }

        Ok(())
    }

    /// Update account balance snapshot used by balance/drawdown guardrails.
    pub fn update_balance(&mut self, balance_cents: i64) -> Result<(), Error> {
        if self.starting_balance_cents.is_none() {
            self.starting_balance_cents = Some(balance_cents);
        }
        self.latest_balance_cents = Some(balance_cents);

        if let Err(e) = self.check_balance_limits() {
            self.kill_switch_engaged = true;
            return Err(e);
        }

        Ok(())
    }

    /// Record a critical execution failure and engage kill switch if threshold is hit.
    pub fn record_critical_failure(&mut self, reason: &str) -> Result<(), Error> {
        self.consecutive_critical_failures = self.consecutive_critical_failures.saturating_add(1);

        let threshold = self.config.kill_switch_disconnect_count.max(1);
        warn!(
            "Critical failure {}/{}: {}",
            self.consecutive_critical_failures, threshold, reason
        );

        if self.consecutive_critical_failures >= threshold {
            self.kill_switch_engaged = true;
            return Err(Error::RiskViolation(format!(
                "Kill switch engaged after {} consecutive critical failures",
                threshold
            )));
        }

        Ok(())
    }

    /// Reset critical failure counter after a healthy execution path.
    pub fn clear_critical_failures(&mut self) {
        self.consecutive_critical_failures = 0;
    }

    pub fn kill_switch_engaged(&self) -> bool {
        self.kill_switch_engaged
    }

    /// Check if an arb opportunity is safe to execute.
    ///
    /// `estimated_entry_debit_cents` should be a conservative estimate of
    /// immediate cash spent on entry (for buy-set style trades). This allows
    /// reserve protection before order submission.
    pub fn check(
        &mut self,
        opp: &ArbOpportunity,
        estimated_entry_debit_cents: i64,
    ) -> Result<(), Error> {
        if self.kill_switch_engaged {
            return Err(Error::RiskViolation(
                "Kill switch engaged; refusing new orders".into(),
            ));
        }
        if opp.qty <= 0 {
            return Err(Error::RiskViolation(
                "Invalid quantity; must be positive".into(),
            ));
        }
        if opp.legs.is_empty() {
            return Err(Error::RiskViolation(
                "Invalid opportunity with zero legs".into(),
            ));
        }
        self.check_balance_limits()?;

        if estimated_entry_debit_cents < 0 {
            return Err(Error::RiskViolation(
                "Estimated entry debit cannot be negative".into(),
            ));
        }
        if estimated_entry_debit_cents > 0 {
            let balance = self.latest_balance_cents.ok_or_else(|| {
                Error::RiskViolation(
                    "Account balance has not been initialized; refusing live execution".into(),
                )
            })?;
            let projected = balance - estimated_entry_debit_cents;
            if projected < self.config.min_balance_cents {
                return Err(Error::RiskViolation(format!(
                    "Projected balance below minimum after trade: {} - {} = {} < {}",
                    balance, estimated_entry_debit_cents, projected, self.config.min_balance_cents
                )));
            }
        }

        let now = Instant::now();

        // 1. Check attempt frequency (spam protection).
        let history = self
            .attempt_history
            .entry(opp.group_event_ticker.clone())
            .or_default();

        Self::prune_rolling_window(history, now);

        if history.len() >= self.config.max_attempts_per_group_per_min as usize {
            warn!("{}: max attempts limit reached", opp.group_event_ticker);
            return Err(Error::RiskViolation("Too many attempts per minute".into()));
        }

        Self::prune_rolling_window(&mut self.order_history, now);
        let projected_orders = self.order_history.len().saturating_add(opp.legs.len());
        if projected_orders > self.config.max_orders_per_minute as usize {
            return Err(Error::RiskViolation(format!(
                "Order rate limit: {} + {} > {} orders/min",
                self.order_history.len(),
                opp.legs.len(),
                self.config.max_orders_per_minute
            )));
        }

        // 2. Check Event Exposure.
        let current_event_exposure = *self
            .event_exposure
            .get(&opp.group_event_ticker)
            .unwrap_or(&0);

        // Conservative exposure add: for arb, we are hedged, but gross exposure matters
        // for "what if one leg fails".
        // Gross exposure of the trade = qty * payout (100) per leg?
        // Or just the max liability.
        // For Buy-Set, max liability is Cost.
        // For Sell-Set, max liability is Payout * derived_qty - Premium.
        // Let's use `payout * qty` as a conservative proxy for specific-event risk.
        let trade_risk = 100 * opp.qty;

        if current_event_exposure + trade_risk > self.config.max_exposure_per_event_cents {
            return Err(Error::RiskViolation(format!(
                "Event exposure limit: {} + {} > {}",
                current_event_exposure, trade_risk, self.config.max_exposure_per_event_cents
            )));
        }

        // 3. Check Total Exposure.
        if self.total_exposure + trade_risk > self.config.max_total_exposure_cents {
            return Err(Error::RiskViolation(format!(
                "Total exposure limit: {} + {} > {}",
                self.total_exposure, trade_risk, self.config.max_total_exposure_cents
            )));
        }

        // Approved. Record the attempt.
        history.push_back(now);
        for _ in 0..opp.legs.len() {
            self.order_history.push_back(now);
        }

        Ok(())
    }

    /// Record a successful execution to update exposure tracking.
    pub fn record_execution(&mut self, opp: &ArbOpportunity) {
        let trade_risk = 100 * opp.qty;
        *self
            .event_exposure
            .entry(opp.group_event_ticker.clone())
            .or_default() += trade_risk;
        self.total_exposure += trade_risk;

        info!(
            "Risk updated: event_exposure={} total={}",
            self.event_exposure[&opp.group_event_ticker], self.total_exposure
        );
    }

    /// Reset exposure tracking (e.g. after position reconciliation).
    pub fn reset_exposure(&mut self) {
        self.event_exposure.clear();
        self.total_exposure = 0;
    }
}

#[cfg(test)]
mod tests {
    use common::Side;

    use crate::arb::{ArbDirection, ArbLeg, ArbOpportunity};

    use super::*;

    fn test_config() -> ArbRiskConfig {
        ArbRiskConfig {
            max_exposure_per_event_cents: 10_000,
            max_total_exposure_cents: 50_000,
            max_attempts_per_group_per_min: 10,
            max_unwind_loss_cents: 100,
            kill_switch_disconnect_count: 2,
            max_orders_per_minute: 3,
            min_balance_cents: 100,
            max_daily_loss_cents: 200,
        }
    }

    fn make_opp(legs: usize, qty: i64) -> ArbOpportunity {
        ArbOpportunity {
            group_event_ticker: "EVT-TEST".into(),
            direction: ArbDirection::BuySet,
            qty,
            gross_edge_cents: 10,
            net_edge_cents: 5,
            legs: (0..legs)
                .map(|idx| ArbLeg {
                    ticker: format!("MKT-{}", idx),
                    price_cents: 40,
                    side: Side::Yes,
                })
                .collect(),
            reason: "test".into(),
        }
    }

    #[test]
    fn test_order_rate_limit_blocks_excess_orders() {
        let mut risk = ArbRiskManager::new(test_config());
        risk.update_balance(1_000).unwrap();

        let opp = make_opp(2, 1);
        assert!(risk.check(&opp, 0).is_ok());

        let err = risk.check(&opp, 0).unwrap_err();
        assert!(err.to_string().contains("Order rate limit"));
    }

    #[test]
    fn test_balance_drawdown_engages_kill_switch() {
        let mut risk = ArbRiskManager::new(test_config());
        risk.update_balance(1_000).unwrap();

        let err = risk.update_balance(750).unwrap_err();
        assert!(err.to_string().contains("Daily drawdown limit breached"));
        assert!(risk.kill_switch_engaged());
    }

    #[test]
    fn test_consecutive_critical_failures_engage_kill_switch() {
        let mut risk = ArbRiskManager::new(test_config());
        risk.update_balance(1_000).unwrap();

        assert!(risk.record_critical_failure("first failure").is_ok());
        let err = risk.record_critical_failure("second failure").unwrap_err();
        assert!(err.to_string().contains("Kill switch engaged"));
        assert!(risk.kill_switch_engaged());
    }

    #[test]
    fn test_projected_balance_guard_blocks_cash_drain() {
        let mut risk = ArbRiskManager::new(test_config());
        risk.update_balance(1_000).unwrap();

        let opp = make_opp(2, 1);
        let err = risk.check(&opp, 950).unwrap_err();
        assert!(err
            .to_string()
            .contains("Projected balance below minimum after trade"));
    }
}
