//! Fee model for arbitrage — wraps the shared FeeSchedule with
//! set-level cost calculations and EV discounting.

use common::FeeSchedule;

use crate::quotes::MarketQuote;

/// Arb-specific fee model.
#[derive(Debug, Clone)]
pub struct ArbFeeModel {
    pub schedule: FeeSchedule,
    /// Slippage buffer in cents per leg.
    pub slippage_buffer: i64,
}

impl ArbFeeModel {
    pub fn new(slippage_buffer: i64) -> Self {
        Self {
            schedule: FeeSchedule::weather(), // Same fee formula for all Kalshi markets.
            slippage_buffer,
        }
    }

    /// Net cost to buy YES on all outcomes (cents).
    ///
    /// `Σ (ask_i + slippage) + Σ fee(qty, ask_i + slippage)`
    pub fn net_buy_set_cost(&self, quotes: &[MarketQuote], qty: i64) -> i64 {
        let mut total = 0i64;
        for q in quotes {
            let effective_ask = q.yes_ask + self.slippage_buffer;
            let fee = self.schedule.taker_fee_cents(qty, effective_ask);
            total += effective_ask * qty + fee;
        }
        total
    }

    /// Net revenue from selling YES on all outcomes (cents).
    ///
    /// `Σ (bid_i - slippage) × qty - Σ fee(qty, bid_i - slippage)`
    ///
    /// Returns `None` when any leg has non-positive executable bid after
    /// slippage, since that leg cannot be sold with a valid positive limit.
    pub fn net_sell_set_revenue(&self, quotes: &[MarketQuote], qty: i64) -> Option<i64> {
        let mut total = 0i64;
        for q in quotes {
            let effective_bid = q.yes_bid - self.slippage_buffer;
            if effective_bid <= 0 {
                return None;
            }
            let fee = self.schedule.taker_fee_cents(qty, effective_bid);
            total += effective_bid * qty - fee;
        }
        Some(total)
    }

    /// Per-contract net cost for buying the complete set.
    pub fn per_contract_buy_cost(&self, quotes: &[MarketQuote]) -> i64 {
        let mut total = 0i64;
        for q in quotes {
            let effective_ask = q.yes_ask + self.slippage_buffer;
            let fee_per = self.schedule.per_contract_taker_fee(effective_ask).ceil() as i64;
            total += effective_ask + fee_per;
        }
        total
    }

    /// Per-contract net revenue for selling the complete set.
    ///
    /// Returns `None` when any leg has non-positive executable bid after
    /// slippage.
    pub fn per_contract_sell_revenue(&self, quotes: &[MarketQuote]) -> Option<i64> {
        let mut total = 0i64;
        for q in quotes {
            let effective_bid = q.yes_bid - self.slippage_buffer;
            if effective_bid <= 0 {
                return None;
            }
            let fee_per = self.schedule.per_contract_taker_fee(effective_bid).ceil() as i64;
            total += effective_bid - fee_per;
        }
        Some(total)
    }

    /// EV-discounted payout for a non-exhaustive set.
    ///
    /// If there's a probability `p_void` that no outcome resolves YES
    /// (tie/void), the expected payout is `100 * (1 - p_void)`.
    pub fn ev_discounted_payout(p_void: f64) -> i64 {
        (100.0 * (1.0 - p_void)).floor() as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_quotes(pairs: &[(i64, i64)]) -> Vec<MarketQuote> {
        pairs
            .iter()
            .enumerate()
            .map(|(i, (bid, ask))| MarketQuote {
                ticker: format!("MKT-{}", i),
                yes_bid: *bid,
                yes_ask: *ask,
                last_price: (bid + ask) / 2,
                volume: 100,
                age_ms: 0,
            })
            .collect()
    }

    #[test]
    fn test_buy_set_cost_simple() {
        let fee_model = ArbFeeModel::new(0); // No slippage
                                             // Two markets: ask=40 and ask=55 → raw cost = 95¢/contract
        let quotes = make_quotes(&[(38, 40), (53, 55)]);
        let cost = fee_model.net_buy_set_cost(&quotes, 1);
        // Cost = (40 + fee(1,40)) + (55 + fee(1,55))
        // fee(1,40) = ceil(0.07 * 1 * 0.4 * 0.6 * 100) = ceil(1.68) = 2
        // fee(1,55) = ceil(0.07 * 1 * 0.55 * 0.45 * 100) = ceil(1.7325) = 2
        // total = 40 + 2 + 55 + 2 = 99
        assert!(cost > 95, "Cost should include fees: {}", cost);
    }

    #[test]
    fn test_sell_set_revenue_simple() {
        let fee_model = ArbFeeModel::new(0);
        // Two markets: bid=60 and bid=45 → raw revenue = 105¢/contract
        let quotes = make_quotes(&[(60, 65), (45, 50)]);
        let rev = fee_model.net_sell_set_revenue(&quotes, 1).unwrap();
        // Revenue = (60 - fee) + (45 - fee) < 105
        assert!(rev < 105, "Revenue should be reduced by fees: {}", rev);
    }

    #[test]
    fn test_sell_set_revenue_rejects_non_positive_executable_bid() {
        let fee_model = ArbFeeModel::new(1);
        let quotes = make_quotes(&[(0, 10), (60, 65)]);
        assert!(
            fee_model.net_sell_set_revenue(&quotes, 1).is_none(),
            "Any non-positive effective bid should invalidate sell-set revenue",
        );
    }

    #[test]
    fn test_slippage_increases_buy_cost() {
        let no_slip = ArbFeeModel::new(0);
        let with_slip = ArbFeeModel::new(1);
        let quotes = make_quotes(&[(40, 45), (50, 55)]);

        let cost_no_slip = no_slip.net_buy_set_cost(&quotes, 10);
        let cost_with_slip = with_slip.net_buy_set_cost(&quotes, 10);

        assert!(
            cost_with_slip > cost_no_slip,
            "Slippage should increase cost: {} vs {}",
            cost_with_slip,
            cost_no_slip
        );
    }

    #[test]
    fn test_ev_discounted_payout() {
        assert_eq!(ArbFeeModel::ev_discounted_payout(0.0), 100);
        assert_eq!(ArbFeeModel::ev_discounted_payout(0.1), 90);
        assert_eq!(ArbFeeModel::ev_discounted_payout(0.5), 50);
    }

    #[test]
    fn test_arb_detectable_buy_set() {
        // Classic arb: two outcomes, asks sum to < 100
        let fee_model = ArbFeeModel::new(0);
        let quotes = make_quotes(&[(30, 35), (55, 60)]);
        let per_contract_cost = fee_model.per_contract_buy_cost(&quotes);
        // 35 + fee + 60 + fee → should be close to 100
        // If < 100, it's an arb!
        // fee(35) ≈ ceil(0.07 * 0.35 * 0.65 * 100) = ceil(1.5925) = 2
        // fee(60) ≈ ceil(0.07 * 0.60 * 0.40 * 100) = ceil(1.68) = 2
        // total = 35 + 2 + 60 + 2 = 99 → arb of 1¢!
        assert!(
            per_contract_cost <= 100,
            "This should be an arb: cost={} <= 100",
            per_contract_cost
        );
    }
}
