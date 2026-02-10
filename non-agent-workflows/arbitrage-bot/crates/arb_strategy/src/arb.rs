//! Arb evaluator — scans groups for mispricing.
//!
//! Checks for:
//! 1. Buy-set arb: Buy YES on all outcomes for < $1.00 net.
//! 2. Sell-set arb: Sell YES on all outcomes for > $1.00 net.
//! 3. EV-mode: Buy/Sell set with edge against discounted payout (if non-exhaustive).

use tracing::debug;

use crate::fees::ArbFeeModel;
use crate::quotes::{MarketQuote, QuoteBook};
use crate::risk::ArbRiskManager;
use crate::universe::{ArbGroup, EdgeCase};

// ── Public Types ──────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ArbOpportunity {
    /// The group being arbitraged.
    pub group_event_ticker: String,
    /// Direction: BuySet or SellSet.
    pub direction: ArbDirection,
    /// Number of contracts per leg.
    pub qty: i64,
    /// Gross profit in cents (before fees).
    pub gross_edge_cents: i64,
    /// Net profit in cents (after fees).
    pub net_edge_cents: i64,
    /// The legs to execute.
    pub legs: Vec<ArbLeg>,
    /// Reasoning for logs.
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArbDirection {
    BuySet,
    SellSet,
}

#[derive(Debug, Clone)]
pub struct ArbLeg {
    pub ticker: String,
    pub price_cents: i64,
}

// ── Evaluator ─────────────────────────────────────────────────────────

pub struct ArbEvaluator<'a> {
    fees: &'a ArbFeeModel,
    quotes: &'a QuoteBook,
}

impl<'a> ArbEvaluator<'a> {
    pub fn new(
        fees: &'a ArbFeeModel,
        quotes: &'a QuoteBook,
    ) -> Self {
        Self { fees, quotes }
    }

    /// Evaluate a single group for arb opportunities.
    pub async fn evaluate(
        &self,
        group: &ArbGroup,
        min_profit: i64,
        ev_mode: bool,
        tie_buffer: i64,
        max_age_secs: u64,
        qty: i64,
    ) -> Option<ArbOpportunity> {
        // 1. Get fresh quotes for all markets in the group.
        let market_quotes = self.quotes.group_quotes(&group.tickers).await?;

        // 2. Freshness check.
        if !QuoteBook::all_fresh(&market_quotes, max_age_secs) {
            debug!("{}: stale quotes, skipping", group.event_ticker);
            return None;
        }

        // 3. Liquidity check (top-of-book size).
        // For simplicity, we just check if top-of-book volume >= qty.
        // A real production bot uses orderbook snapshots for depth.
        // Using `volume` field from ticker is NOT size-at-price, it's daily volume!
        // Kalshi ticker WS doesn't give size-at-price directly cleanly in all messages.
        // We will assume sufficient liquidity if spread is tight and rely on FOK to fail safely.
        // TODO: Implement OB-deltas for true size check.

        // 4. Determine payout target.
        let (p_void, required_buffer) = if group.is_exhaustive {
            (0.0, 0)
        } else {
            if !ev_mode {
                debug!("{}: non-exhaustive group, EV mode disabled", group.event_ticker);
                return None;
            }
            // Heuristic p_void based on edge case type.
            let p = match group.edge_case {
                Some(EdgeCase::TiePossible) => 0.05, // Assign 5% prob to tie if not listed
                Some(EdgeCase::VoidPossible) => 0.02, // Assign 2% to void
                Some(EdgeCase::PartialSet) => 0.10,   // Partial sets are risky!
                None => 0.0,
            };
            (p, tie_buffer)
        };

        let payout = ArbFeeModel::ev_discounted_payout(p_void);

        // 5. Check Buy-Set Arb (Buy all YES).
        let buy_cost = self.fees.net_buy_set_cost(&market_quotes, qty);
        // Revenue is fixed payout * qty.
        let buy_revenue = payout * qty;
        let buy_net_profit = buy_revenue - buy_cost;

        if buy_net_profit >= (min_profit + required_buffer) * qty {
            return Some(ArbOpportunity {
                group_event_ticker: group.event_ticker.clone(),
                direction: ArbDirection::BuySet,
                qty,
                gross_edge_cents: (payout * qty) - buy_cost, // Approx
                net_edge_cents: buy_net_profit,
                legs: market_quotes
                    .iter()
                    .map(|q| ArbLeg {
                        ticker: q.ticker.clone(),
                        price_cents: q.yes_ask,
                    })
                    .collect(),
                reason: format!(
                    "BUY-SET: cost ({:.2}/c) < payout ({:.2}/c); net={:.2}",
                    buy_cost as f64 / qty as f64,
                    payout as f64 / qty as f64,
                    buy_net_profit
                ),
            });
        }

        // 6. Check Sell-Set Arb (Sell all YES).
        // Revenue is net_sell_revenue (already fee deducted).
        // Cost is potential payout * qty (liability).
        let sell_revenue = self.fees.net_sell_set_revenue(&market_quotes, qty);
        let sell_cost = payout * qty; // Max liability
        let sell_net_profit = sell_revenue - sell_cost;

        if sell_net_profit >= (min_profit + required_buffer) * qty {
            return Some(ArbOpportunity {
                group_event_ticker: group.event_ticker.clone(),
                direction: ArbDirection::SellSet,
                qty,
                gross_edge_cents: sell_revenue - (payout * qty), // Approx
                net_edge_cents: sell_net_profit,
                legs: market_quotes
                    .iter()
                    .map(|q| ArbLeg {
                        ticker: q.ticker.clone(),
                        price_cents: q.yes_bid,
                    })
                    .collect(),
                reason: format!(
                    "SELL-SET: revenue ({:.2}/c) > liability ({:.2}/c); net={:.2}",
                    sell_revenue as f64 / qty as f64,
                    payout as f64 / qty as f64,
                    sell_net_profit
                ),
            });
        }

        None
    }
}
