//! Risk manager — enforces position limits, concentration limits,
//! drawdown protection, and order throttling.

use std::collections::VecDeque;
use std::time::Instant;

use common::config::RiskConfig;
use common::{Action, Error, OrderIntent, Position};
use tracing::{info, warn};

/// Risk manager that validates order intents before execution.
#[derive(Debug)]
pub struct RiskManager {
    /// Configuration parameters.
    pub config: RiskConfig,
    /// Balance at session start (for drawdown tracking).
    session_start_balance: Option<i64>,
    /// Timestamps of recently placed orders (sliding window for throttle).
    order_timestamps: VecDeque<Instant>,
}

impl RiskManager {
    pub fn new(config: RiskConfig) -> Self {
        Self {
            config,
            session_start_balance: None,
            order_timestamps: VecDeque::new(),
        }
    }

    /// Legacy constructor for backward compatibility.
    pub fn from_max_position(max_position_cents: i64) -> Self {
        Self::new(RiskConfig {
            max_position_cents,
            ..Default::default()
        })
    }

    /// Set the session start balance (call once at startup after auth).
    pub fn set_session_balance(&mut self, balance: i64) {
        if self.session_start_balance.is_none() {
            self.session_start_balance = Some(balance);
            info!("Risk manager: session start balance = {}¢", balance);
        }
    }

    /// Record that an order was placed (for throttle tracking).
    pub fn record_order(&mut self) {
        self.order_timestamps.push_back(Instant::now());
        // Prune old entries (>60s ago).
        let cutoff = Instant::now() - std::time::Duration::from_secs(60);
        while self.order_timestamps.front().is_some_and(|t| *t < cutoff) {
            self.order_timestamps.pop_front();
        }
    }

    /// Check whether the order frequency is within limits.
    fn check_order_throttle(&self) -> Result<(), Error> {
        let cutoff = Instant::now() - std::time::Duration::from_secs(60);
        let recent_count = self
            .order_timestamps
            .iter()
            .filter(|t| **t >= cutoff)
            .count() as u32;

        if recent_count >= self.config.max_orders_per_minute {
            return Err(Error::RiskViolation(format!(
                "Order throttle: {} orders in last 60s >= max {}",
                recent_count, self.config.max_orders_per_minute
            )));
        }
        Ok(())
    }

    /// Check drawdown circuit-breaker.
    fn check_drawdown(&self, current_balance: i64) -> Result<(), Error> {
        if let Some(start_balance) = self.session_start_balance {
            let loss = start_balance - current_balance;
            if loss > self.config.max_daily_loss_cents {
                return Err(Error::RiskViolation(format!(
                    "Drawdown circuit-breaker: loss={}¢ exceeds max={}¢ (start={}¢, now={}¢)",
                    loss, self.config.max_daily_loss_cents, start_balance, current_balance
                )));
            }
        }
        Ok(())
    }

    /// Check per-city concentration limit.
    fn check_city_concentration(
        &self,
        intent: &OrderIntent,
        positions: &[Position],
    ) -> Result<(), Error> {
        // Extract city prefix from ticker (e.g., "KXHIGHNYC" from "KXHIGHNYC-26FEB10-B50").
        let city_prefix = extract_city_prefix(&intent.ticker);

        let mut city_exposure: i64 = positions
            .iter()
            .filter(|p| extract_city_prefix(&p.ticker) == city_prefix)
            .map(|p| p.market_exposure.abs())
            .sum();

        city_exposure += intent.price_cents * intent.count;

        if city_exposure > self.config.max_city_exposure_cents {
            return Err(Error::RiskViolation(format!(
                "City concentration limit for '{}': exposure={}¢ > max={}¢",
                city_prefix, city_exposure, self.config.max_city_exposure_cents
            )));
        }
        Ok(())
    }

    /// Check whether an order intent passes all risk checks.
    ///
    /// Returns `Ok(())` if the order is allowed, or `Err` with reason.
    pub fn check_order(
        &self,
        intent: &OrderIntent,
        positions: &[Position],
        balance_cents: i64,
    ) -> Result<(), Error> {
        // For sells, validate the position exists.
        if intent.action == Action::Sell {
            let has_position = positions
                .iter()
                .any(|p| p.ticker == intent.ticker && p.yes_count > 0);
            if !has_position {
                return Err(Error::RiskViolation(format!(
                    "Sell rejected for {}: no position found",
                    intent.ticker
                )));
            }
            return Ok(());
        }

        // Buys go through all checks.

        // 1. Order throttle.
        self.check_order_throttle()?;

        // 2. Drawdown circuit-breaker.
        self.check_drawdown(balance_cents)?;

        // 3. Per-market position limit.
        let current_exposure = positions
            .iter()
            .find(|p| p.ticker == intent.ticker)
            .map(|p| p.market_exposure.abs())
            .unwrap_or(0);

        let order_cost = intent.price_cents * intent.count;
        let new_exposure = current_exposure + order_cost;

        if new_exposure > self.config.max_position_cents {
            return Err(Error::RiskViolation(format!(
                "Position limit exceeded for {}: current={}¢ + order={}¢ = {}¢ > max={}¢",
                intent.ticker,
                current_exposure,
                order_cost,
                new_exposure,
                self.config.max_position_cents
            )));
        }

        // 4. Total portfolio exposure.
        let total_exposure: i64 = positions.iter().map(|p| p.market_exposure.abs()).sum();
        if total_exposure + order_cost > self.config.max_total_exposure_cents {
            return Err(Error::RiskViolation(format!(
                "Total exposure limit exceeded: current={}¢ + order={}¢ > max={}¢",
                total_exposure, order_cost, self.config.max_total_exposure_cents
            )));
        }

        // 5. City concentration.
        self.check_city_concentration(intent, positions)?;

        // 6. Balance check.
        if balance_cents - order_cost < self.config.min_balance_cents {
            return Err(Error::RiskViolation(format!(
                "Insufficient balance: {}¢ - {}¢ < min {}¢",
                balance_cents, order_cost, self.config.min_balance_cents
            )));
        }

        Ok(())
    }

    /// Filter a list of order intents, keeping only those that pass risk checks.
    pub fn filter_intents(
        &self,
        intents: Vec<OrderIntent>,
        positions: &[Position],
        balance_cents: i64,
    ) -> Vec<OrderIntent> {
        let mut approved = Vec::new();
        let mut running_cost = 0i64;

        for intent in intents {
            let adjusted_balance = balance_cents - running_cost;

            match self.check_order(&intent, positions, adjusted_balance) {
                Ok(()) => {
                    if intent.action == Action::Buy {
                        running_cost += intent.price_cents * intent.count;
                    }
                    info!(
                        "RISK APPROVED: {} {} {} @ {}¢ x{} (fee≈{}¢)",
                        match intent.action {
                            Action::Buy => "BUY",
                            Action::Sell => "SELL",
                        },
                        match intent.side {
                            common::Side::Yes => "YES",
                            common::Side::No => "NO",
                        },
                        intent.ticker,
                        intent.price_cents,
                        intent.count,
                        intent.estimated_fee_cents,
                    );
                    approved.push(intent);
                }
                Err(e) => {
                    warn!("RISK REJECTED: {} — {}", intent.ticker, e);
                }
            }
        }

        approved
    }
}

/// Extract the city prefix from a ticker (best-effort).
/// E.g., "KXHIGHNYC-26FEB10-B50" → "KXHIGHNYC"
fn extract_city_prefix(ticker: &str) -> String {
    // Take everything before the first '-'.
    ticker.split('-').next().unwrap_or(ticker).to_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::Side;

    fn default_risk_config() -> RiskConfig {
        RiskConfig {
            max_position_cents: 500,
            max_total_exposure_cents: 5000,
            max_city_exposure_cents: 1500,
            max_daily_loss_cents: 2000,
            max_orders_per_minute: 10,
            min_balance_cents: 100,
        }
    }

    fn make_intent(ticker: &str, price: i64, count: i64) -> OrderIntent {
        OrderIntent {
            ticker: ticker.into(),
            side: Side::Yes,
            action: Action::Buy,
            price_cents: price,
            count,
            reason: "test".into(),
            estimated_fee_cents: 0,
            confidence: 0.9,
        }
    }

    fn make_position(ticker: &str, exposure: i64, yes_count: i64) -> Position {
        Position {
            ticker: ticker.into(),
            yes_count,
            no_count: 0,
            market_exposure: exposure,
            realized_pnl: 0,
        }
    }

    // ── Original tests (preserved) ────────────────────────────────────

    #[test]
    fn test_position_limit_enforced() {
        let rm = RiskManager::new(default_risk_config());

        // Try to buy 30 contracts at 20¢ = 600¢ > 500¢ limit
        let intent = make_intent("TEST-TICKER", 20, 30);
        let result = rm.check_order(&intent, &[], 10000);
        assert!(result.is_err(), "Should reject over-limit position");
    }

    #[test]
    fn test_position_within_limit() {
        let rm = RiskManager::new(default_risk_config());

        // Buy 20 contracts at 20¢ = 400¢ < 500¢ limit
        let intent = make_intent("TEST-TICKER", 20, 20);
        let result = rm.check_order(&intent, &[], 10000);
        assert!(result.is_ok(), "Should allow within-limit position");
    }

    #[test]
    fn test_insufficient_balance() {
        let rm = RiskManager::new(default_risk_config());

        // Try to buy with only 200¢ balance, order cost = 300¢
        let intent = make_intent("TEST-TICKER", 15, 20);
        let result = rm.check_order(&intent, &[], 200);
        assert!(result.is_err(), "Should reject when balance insufficient");
    }

    #[test]
    fn test_sells_with_position_pass() {
        let rm = RiskManager::new(default_risk_config());

        let intent = OrderIntent {
            ticker: "TEST".into(),
            side: Side::Yes,
            action: Action::Sell,
            price_cents: 50,
            count: 5,
            reason: "test sell".into(),
            estimated_fee_cents: 0,
            confidence: 0.9,
        };
        let positions = vec![make_position("TEST", 500, 10)];
        let result = rm.check_order(&intent, &positions, 0);
        assert!(result.is_ok(), "Sells with existing position should pass");
    }

    // ── New tests ─────────────────────────────────────────────────────

    #[test]
    fn test_sell_without_position_rejected() {
        let rm = RiskManager::new(default_risk_config());

        let intent = OrderIntent {
            ticker: "NONEXISTENT".into(),
            side: Side::Yes,
            action: Action::Sell,
            price_cents: 50,
            count: 5,
            reason: "test sell".into(),
            estimated_fee_cents: 0,
            confidence: 0.9,
        };
        let result = rm.check_order(&intent, &[], 10000);
        assert!(result.is_err(), "Sells without position should be rejected");
    }

    #[test]
    fn test_city_concentration_limit() {
        let rm = RiskManager::new(default_risk_config());

        // Already have 1200¢ in KXHIGHNYC markets.
        let positions = vec![
            make_position("KXHIGHNYC-B50", 600, 10),
            make_position("KXHIGHNYC-B55", 600, 10),
        ];

        // Try to add 400¢ more → 1600¢ > 1500¢ city limit.
        let intent = make_intent("KXHIGHNYC-B60", 20, 20);
        let result = rm.check_order(&intent, &positions, 10000);
        assert!(
            result.is_err(),
            "Should reject when city concentration exceeded"
        );
    }

    #[test]
    fn test_city_concentration_different_cities_ok() {
        let rm = RiskManager::new(default_risk_config());

        // 1200¢ in NYC doesn't block a Chicago trade.
        let positions = vec![
            make_position("KXHIGHNYC-B50", 600, 10),
            make_position("KXHIGHNYC-B55", 600, 10),
        ];

        let intent = make_intent("KXHIGHCHI-B30", 20, 20);
        let result = rm.check_order(&intent, &positions, 10000);
        assert!(
            result.is_ok(),
            "Different city should not be blocked by NYC concentration"
        );
    }

    #[test]
    fn test_drawdown_circuit_breaker() {
        let mut rm = RiskManager::new(default_risk_config());
        rm.set_session_balance(10000);

        // Current balance = 7500 → loss = 2500 > max 2000
        let intent = make_intent("TEST-TICKER", 10, 5);
        let result = rm.check_order(&intent, &[], 7500);
        assert!(
            result.is_err(),
            "Should block all buys when drawdown exceeds limit"
        );
    }

    #[test]
    fn test_drawdown_within_limit() {
        let mut rm = RiskManager::new(default_risk_config());
        rm.set_session_balance(10000);

        // Current balance = 9000 → loss = 1000 < max 2000
        let intent = make_intent("TEST-TICKER", 10, 5);
        let result = rm.check_order(&intent, &[], 9000);
        assert!(
            result.is_ok(),
            "Should allow buys when drawdown is within limit"
        );
    }

    #[test]
    fn test_order_throttle() {
        let mut rm = RiskManager::new(RiskConfig {
            max_orders_per_minute: 3,
            ..default_risk_config()
        });

        // Record 3 orders (at the limit).
        rm.record_order();
        rm.record_order();
        rm.record_order();

        let intent = make_intent("TEST-TICKER", 10, 5);
        let result = rm.check_order(&intent, &[], 10000);
        assert!(
            result.is_err(),
            "Should reject when order frequency exceeds limit"
        );
    }

    #[test]
    fn test_total_exposure_limit() {
        let rm = RiskManager::new(default_risk_config());

        // Already have 4800¢ total exposure.
        let positions = vec![
            make_position("A-1", 400, 10),
            make_position("B-1", 400, 10),
            make_position("C-1", 400, 10),
            make_position("D-1", 400, 10),
            make_position("E-1", 400, 10),
            make_position("F-1", 400, 10),
            make_position("G-1", 400, 10),
            make_position("H-1", 400, 10),
            make_position("I-1", 400, 10),
            make_position("J-1", 400, 10),
            make_position("K-1", 400, 10),
            make_position("L-1", 400, 10),
        ];

        // Try to add 300¢ more → 5100¢ > 5000¢ total limit.
        let intent = make_intent("M-1", 15, 20);
        let result = rm.check_order(&intent, &positions, 10000);
        assert!(
            result.is_err(),
            "Should reject when total exposure limit exceeded"
        );
    }
}
