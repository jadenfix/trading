//! Risk manager — enforces position limits and trade caps.

use common::{Error, OrderIntent, Position, Action};
use tracing::{info, warn};

/// Risk manager that validates order intents before execution.
#[derive(Debug, Clone)]
pub struct RiskManager {
    /// Max position per market in cents.
    pub max_position_cents: i64,
    /// Max total portfolio exposure in cents.
    pub max_total_exposure_cents: i64,
    /// Minimum balance to maintain (cents).
    pub min_balance_cents: i64,
}

impl RiskManager {
    pub fn new(max_position_cents: i64) -> Self {
        Self {
            max_position_cents,
            max_total_exposure_cents: max_position_cents * 20, // 20 markets max
            min_balance_cents: 100, // Keep at least $1
        }
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
        // For sells, we're reducing exposure — always allow.
        if intent.action == Action::Sell {
            return Ok(());
        }

        // 1. Check per-market position limit.
        let current_exposure = positions
            .iter()
            .find(|p| p.ticker == intent.ticker)
            .map(|p| p.market_exposure.abs())
            .unwrap_or(0);

        let order_cost = intent.price_cents * intent.count;
        let new_exposure = current_exposure + order_cost;

        if new_exposure > self.max_position_cents {
            return Err(Error::RiskViolation(format!(
                "Position limit exceeded for {}: current={}¢ + order={}¢ = {}¢ > max={}¢",
                intent.ticker, current_exposure, order_cost, new_exposure, self.max_position_cents
            )));
        }

        // 2. Check total portfolio exposure.
        let total_exposure: i64 = positions.iter().map(|p| p.market_exposure.abs()).sum();
        if total_exposure + order_cost > self.max_total_exposure_cents {
            return Err(Error::RiskViolation(format!(
                "Total exposure limit exceeded: current={}¢ + order={}¢ > max={}¢",
                total_exposure, order_cost, self.max_total_exposure_cents
            )));
        }

        // 3. Check balance.
        if balance_cents - order_cost < self.min_balance_cents {
            return Err(Error::RiskViolation(format!(
                "Insufficient balance: {}¢ - {}¢ < min {}¢",
                balance_cents, order_cost, self.min_balance_cents
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
                        "RISK APPROVED: {} {} {} @ {}¢ x{}",
                        match intent.action { Action::Buy => "BUY", Action::Sell => "SELL" },
                        match intent.side { common::Side::Yes => "YES", common::Side::No => "NO" },
                        intent.ticker,
                        intent.price_cents,
                        intent.count,
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

#[cfg(test)]
mod tests {
    use super::*;
    use common::{Side, Action};

    fn make_intent(ticker: &str, price: i64, count: i64) -> OrderIntent {
        OrderIntent {
            ticker: ticker.into(),
            side: Side::Yes,
            action: Action::Buy,
            price_cents: price,
            count,
            reason: "test".into(),
        }
    }

    #[test]
    fn test_position_limit_enforced() {
        let rm = RiskManager::new(500); // $5 max per market

        // Try to buy 30 contracts at 20¢ = 600¢ > 500¢ limit
        let intent = make_intent("TEST-TICKER", 20, 30);
        let result = rm.check_order(&intent, &[], 10000);
        assert!(result.is_err(), "Should reject over-limit position");
    }

    #[test]
    fn test_position_within_limit() {
        let rm = RiskManager::new(500);

        // Buy 20 contracts at 20¢ = 400¢ < 500¢ limit
        let intent = make_intent("TEST-TICKER", 20, 20);
        let result = rm.check_order(&intent, &[], 10000);
        assert!(result.is_ok(), "Should allow within-limit position");
    }

    #[test]
    fn test_insufficient_balance() {
        let rm = RiskManager::new(500);

        // Try to buy with only 200¢ balance, order cost = 300¢
        let intent = make_intent("TEST-TICKER", 15, 20);
        let result = rm.check_order(&intent, &[], 200);
        assert!(result.is_err(), "Should reject when balance insufficient");
    }

    #[test]
    fn test_sells_always_pass() {
        let rm = RiskManager::new(500);

        let intent = OrderIntent {
            ticker: "TEST".into(),
            side: Side::Yes,
            action: Action::Sell,
            price_cents: 50,
            count: 100,
            reason: "test sell".into(),
        };
        let result = rm.check_order(&intent, &[], 0);
        assert!(result.is_ok(), "Sells should always pass risk checks");
    }
}
