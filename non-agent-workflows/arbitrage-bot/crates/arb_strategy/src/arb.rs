//! Arb evaluator — scans groups for mispricing.
//!
//! Checks for:
//! 1. Buy-set arb: Buy YES on all outcomes for < $1.00 net.
//! 2. Sell-set arb: Sell YES on all outcomes for > $1.00 net.
//! 3. Optional EV-mode for non-exhaustive sets (disabled in guaranteed-only mode).

use tracing::debug;

use crate::fees::ArbFeeModel;
use crate::quotes::QuoteBook;
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

use common::Side;

#[derive(Debug, Clone)]
pub struct ArbLeg {
    pub ticker: String,
    pub price_cents: i64,
    pub side: Side,
}

// ── Evaluator ─────────────────────────────────────────────────────────

pub struct ArbEvaluator<'a> {
    fees: &'a ArbFeeModel,
    quotes: &'a QuoteBook,
}

impl<'a> ArbEvaluator<'a> {
    pub fn new(fees: &'a ArbFeeModel, quotes: &'a QuoteBook) -> Self {
        Self { fees, quotes }
    }

    /// Evaluate a single group for arb opportunities.
    pub async fn evaluate(
        &self,
        group: &ArbGroup,
        min_profit: i64,
        ev_mode: bool,
        guaranteed_only: bool,
        tie_buffer: i64,
        max_age_secs: u64,
        qty: i64,
    ) -> Option<ArbOpportunity> {
        let required_profit_per_contract = min_profit.max(1);

        // 1. Get fresh quotes for all markets in the group.
        let market_quotes = self.quotes.group_quotes(&group.tickers).await?;

        // 2. Freshness check.
        if !QuoteBook::all_fresh(&market_quotes, max_age_secs) {
            debug!("{}: stale quotes, skipping", group.event_ticker);
            return None;
        }

        // Non-mutually-exclusive/partial sets can have multiple YES outcomes
        // resolve at once, so set-arb payout assumptions are invalid.
        if matches!(group.edge_case, Some(EdgeCase::PartialSet)) {
            debug!(
                "{}: partial/non-mutually-exclusive set, skipping",
                group.event_ticker
            );
            return None;
        }

        // 3. Liquidity check (top-of-book size).
        // For simplicity, we just check if top-of-book volume >= qty.
        // A real production bot uses orderbook snapshots for depth.
        // Using `volume` field from ticker is NOT size-at-price, it's daily volume!
        // Kalshi ticker WS doesn't give size-at-price directly cleanly in all messages.
        // We will assume sufficient liquidity if spread is tight and rely on FOK to fail safely.
        // TODO: Implement OB-deltas for true size check.

        // 3. Liquidity check (top-of-book size).
        // ... (existing comments)

        if market_quotes.len() == 1 {
            // Single-market Arb: Check YES + NO < Payout
            // "Buy NO" means taking the YES Bid.
            // Cost of Buying NO = 100 - YES Bid.
            let quote = &market_quotes[0];

            let no_ask = 100 - quote.yes_bid;

            let yes_cost =
                self.fees.schedule.taker_fee_cents(qty, quote.yes_ask) + (quote.yes_ask * qty);
            let no_cost = self.fees.schedule.taker_fee_cents(qty, no_ask) + (no_ask * qty);
            let total_cost = yes_cost + no_cost;

            // Payout for YES+NO is always $1.00 (100 cents) per unit, if strictly binary.
            let payout = 100 * qty;

            let net_profit = payout - total_cost;

            if net_profit >= required_profit_per_contract * qty {
                return Some(ArbOpportunity {
                    group_event_ticker: group.event_ticker.clone(),
                    direction: ArbDirection::BuySet, // Buying the "set" of YES+NO
                    qty,
                    gross_edge_cents: payout - (quote.yes_ask * qty + no_ask * qty),
                    net_edge_cents: net_profit,
                    legs: vec![
                        ArbLeg {
                            ticker: quote.ticker.clone(),
                            price_cents: quote.yes_ask,
                            side: Side::Yes,
                        }, // Buy YES
                        ArbLeg {
                            ticker: quote.ticker.clone(),
                            price_cents: no_ask,
                            side: Side::No,
                        }, // Buy NO
                    ],
                    reason: format!(
                        "SINGLE-MKT ARB: Yes({}¢)+No({}¢)+Fees({}¢) < 100¢; net={}",
                        quote.yes_ask,
                        no_ask,
                        (yes_cost + no_cost - (quote.yes_ask + no_ask) * qty) / qty,
                        net_profit
                    ),
                });
            }
            return None;
        }

        // 4. Determine payout target.
        let (payout, required_buffer) = if group.is_exhaustive {
            (100, 0)
        } else {
            if guaranteed_only {
                debug!(
                    "{}: non-exhaustive group, guaranteed-only mode enabled",
                    group.event_ticker
                );
                return None;
            }
            if !ev_mode {
                debug!(
                    "{}: non-exhaustive group, EV mode disabled",
                    group.event_ticker
                );
                return None;
            }
            // Heuristic p_void based on edge case type.
            let p = match group.edge_case {
                Some(EdgeCase::TiePossible) => 0.05, // Assign 5% prob to tie if not listed
                Some(EdgeCase::VoidPossible) => 0.02, // Assign 2% to void
                Some(EdgeCase::PartialSet) => 0.10,  // Partial sets are risky!
                None => 0.0,
            };
            (ArbFeeModel::ev_discounted_payout(p), tie_buffer)
        };

        // 5. Check Buy-Set Arb (Buy all YES).
        let buy_cost = self.fees.net_buy_set_cost(&market_quotes, qty);
        // Revenue is fixed payout * qty.
        let buy_revenue = payout * qty;
        let buy_net_profit = buy_revenue - buy_cost;

        if buy_net_profit >= (required_profit_per_contract + required_buffer) * qty {
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
                        side: Side::Yes,
                    })
                    .collect(),
                reason: format!(
                    "BUY-SET: cost ({:.2}/c) < payout ({:.2}/c); net={}¢",
                    buy_cost as f64 / qty as f64,
                    payout as f64,
                    buy_net_profit
                ),
            });
        }

        // 6. Check Sell-Set Arb (Sell all YES).
        // Revenue is net_sell_revenue (already fee deducted).
        // Cost is potential payout * qty (liability).
        let sell_revenue = match self.fees.net_sell_set_revenue(&market_quotes, qty) {
            Some(rev) => rev,
            None => {
                debug!(
                    "{}: sell-set invalid; non-positive executable bid after slippage",
                    group.event_ticker
                );
                return None;
            }
        };
        let sell_cost = payout * qty; // Max liability
        let sell_net_profit = sell_revenue - sell_cost;

        if sell_net_profit >= (required_profit_per_contract + required_buffer) * qty {
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
                        side: Side::Yes,
                    })
                    .collect(),
                reason: format!(
                    "SELL-SET: revenue ({:.2}/c) > liability ({:.2}/c); net={}¢",
                    sell_revenue as f64 / qty as f64,
                    payout as f64,
                    sell_net_profit
                ),
            });
        }

        None
    }
}
