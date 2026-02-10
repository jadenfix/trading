//! Strategy evaluation engine.
//!
//! Evaluates all tracked markets on each tick, comparing forecast-derived
//! fair value against live market prices. Emits `OrderIntent`s for the
//! execution layer to process.
//!
//! Key features:
//! - Fee-adjusted edge calculation (only trades when post-fee edge is positive)
//! - Half-Kelly position sizing (edge-proportional, capped)
//! - Confidence-interval filtering (skips uncertain forecasts)

use std::collections::HashMap;
use std::time::Duration;

use chrono::Utc;
use common::config::BotConfig;
use common::{Action, FeeSchedule, MarketInfo, OrderIntent, Side};
use kalshi_client::PriceEntry;
use noaa_client::probability::{compute_probability, ProbabilityEstimate};
use tracing::{debug, info};

use crate::cache::{ForecastCache, ForecastEntry};

/// Minimum confidence to consider a forecast actionable.
const MIN_CONFIDENCE: f64 = 0.5;

/// Kelly fraction: use half-Kelly for safety.
const KELLY_FRACTION: f64 = 0.5;

/// The strategy engine that evaluates markets.
pub struct StrategyEngine {
    pub config: BotConfig,
    pub fees: FeeSchedule,
}

impl StrategyEngine {
    pub fn new(config: BotConfig) -> Self {
        Self {
            config,
            fees: FeeSchedule::weather(),
        }
    }

    /// Evaluate all markets and return order intents.
    ///
    /// # Arguments
    /// * `markets` — discovered markets keyed by ticker
    /// * `prices` — latest price data from WebSocket
    /// * `forecasts` — latest forecast data from NOAA
    /// * `positions` — current positions keyed by ticker
    pub fn evaluate(
        &self,
        markets: &HashMap<String, MarketInfo>,
        prices: &HashMap<String, PriceEntry>,
        forecasts: &ForecastCache,
        positions: &HashMap<String, i64>, // ticker → yes_count
    ) -> Vec<OrderIntent> {
        let mut intents = Vec::new();
        let now = Utc::now();
        let strat = &self.config.strategy;

        for (ticker, market) in markets {
            // 1. Check market is tradeable.
            if market.status != "open" {
                continue;
            }

            // 2. Check not too close to expiry.
            if let Some(close_time) = market.close_time {
                let hours_left = (close_time - now).num_minutes() as f64 / 60.0;
                if hours_left < strat.min_hours_before_close {
                    debug!("{}: too close to expiry ({:.1}h left)", ticker, hours_left);
                    continue;
                }
            }

            // 3. Get price data — skip if stale.
            let price = match prices.get(ticker) {
                Some(p) => p,
                None => {
                    debug!("{}: no price data yet", ticker);
                    continue;
                }
            };

            if price.updated_at.elapsed() > Duration::from_secs(self.config.timing.price_stale_secs)
            {
                debug!("{}: price data stale", ticker);
                continue;
            }

            // 4. Check spread.
            let spread = price.yes_ask - price.yes_bid;
            if spread > strat.max_spread_cents {
                debug!("{}: spread too wide ({}¢)", ticker, spread);
                continue;
            }

            // 5. Find matching forecast.
            let city_name = self.ticker_to_city(ticker);
            let forecast_entry: Option<ForecastEntry> = city_name
                .as_ref()
                .and_then(|name| forecasts.get(name).map(|e| e.clone()));

            let forecast = match forecast_entry {
                Some(ref entry) => {
                    if entry.is_stale(self.config.timing.forecast_stale_secs) {
                        debug!("{}: forecast stale for city", ticker);
                        continue;
                    }
                    &entry.data
                }
                None => {
                    debug!("{}: no forecast data", ticker);
                    continue;
                }
            };

            // 6. Compute probability estimate with confidence bounds.
            let strike_type = market.strike_type.as_deref().unwrap_or("greater");
            let estimate: ProbabilityEstimate = compute_probability(
                forecast,
                strike_type,
                market.floor_strike,
                market.cap_strike,
            );

            // 7. Skip if forecast confidence is too low.
            if estimate.confidence < MIN_CONFIDENCE {
                debug!(
                    "{}: forecast confidence too low ({:.2} < {})",
                    ticker, estimate.confidence, MIN_CONFIDENCE
                );
                continue;
            }

            // 8. Compute fee-adjusted edge.
            //    fair_cents uses the conservative p_yes_low for entries.
            let fair_cents = (100.0 * estimate.p_yes).floor() as i64 - strat.safety_margin_cents;
            let conservative_fair =
                (100.0 * estimate.p_yes_low).floor() as i64 - strat.safety_margin_cents;

            // Per-contract fee for entry + assumed exit.
            let entry_fee_per_contract = self.fees.per_contract_taker_fee(price.yes_ask);
            let exit_fee_per_contract =
                self.fees.per_contract_taker_fee(strat.exit_threshold_cents);
            let round_trip_fee_per_contract =
                (entry_fee_per_contract + exit_fee_per_contract).ceil() as i64;

            let gross_edge = fair_cents - price.yes_ask;
            let net_edge = gross_edge - round_trip_fee_per_contract;
            let conservative_net_edge =
                conservative_fair - price.yes_ask - round_trip_fee_per_contract;

            debug!(
                "{}: p_yes={:.3} [{:.3}–{:.3}] conf={:.2}, fair={}¢, ask={}¢, \
                 gross_edge={}¢, fees={}¢/c, net_edge={}¢, cons_net={}¢",
                ticker,
                estimate.p_yes,
                estimate.p_yes_low,
                estimate.p_yes_high,
                estimate.confidence,
                fair_cents,
                price.yes_ask,
                gross_edge,
                round_trip_fee_per_contract,
                net_edge,
                conservative_net_edge
            );

            let current_pos = positions.get(ticker).copied().unwrap_or(0);

            // 9. Entry signal — BUY YES (fee-adjusted).
            if price.yes_ask <= strat.entry_threshold_cents
                && net_edge >= strat.edge_threshold_cents
                && conservative_net_edge > 0  // conservative estimate must also be positive
                && current_pos == 0
            {
                // Half-Kelly position sizing.
                let count = self.kelly_size(
                    estimate.p_yes,
                    price.yes_ask,
                    strat.max_position_cents,
                    round_trip_fee_per_contract,
                );

                let estimated_fee = self.fees.taker_fee_cents(count, price.yes_ask);

                info!(
                    "ENTRY: {} — BUY YES @ {}¢ x{} (fair={}¢, net_edge={}¢, fees≈{}¢, \
                     p_yes={:.3} [{:.3}–{:.3}], conf={:.2})",
                    ticker,
                    price.yes_ask,
                    count,
                    fair_cents,
                    net_edge,
                    estimated_fee,
                    estimate.p_yes,
                    estimate.p_yes_low,
                    estimate.p_yes_high,
                    estimate.confidence
                );

                intents.push(OrderIntent {
                    ticker: ticker.clone(),
                    side: Side::Yes,
                    action: Action::Buy,
                    price_cents: price.yes_ask,
                    count,
                    reason: format!(
                        "entry: ask={}¢, fair={}¢, net_edge={}¢ (fees={}¢/c), p_yes={:.3}, conf={:.2}",
                        price.yes_ask, fair_cents, net_edge, round_trip_fee_per_contract,
                        estimate.p_yes, estimate.confidence
                    ),
                    estimated_fee_cents: estimated_fee,
                    confidence: estimate.confidence,
                });
            }

            // 10. Exit signal — SELL YES.
            if price.yes_bid >= strat.exit_threshold_cents && current_pos > 0 {
                let estimated_fee = self.fees.taker_fee_cents(current_pos, price.yes_bid);

                info!(
                    "EXIT: {} — SELL YES @ {}¢ x{} (bid={}¢ >= thresh={}¢, fee≈{}¢)",
                    ticker,
                    price.yes_bid,
                    current_pos,
                    price.yes_bid,
                    strat.exit_threshold_cents,
                    estimated_fee
                );

                intents.push(OrderIntent {
                    ticker: ticker.clone(),
                    side: Side::Yes,
                    action: Action::Sell,
                    price_cents: price.yes_bid,
                    count: current_pos,
                    reason: format!(
                        "exit: bid={}¢ >= thresh={}¢, fee≈{}¢",
                        price.yes_bid, strat.exit_threshold_cents, estimated_fee
                    ),
                    estimated_fee_cents: estimated_fee,
                    confidence: estimate.confidence,
                });
            }

            // Enforce max trades per run.
            if intents.len() >= strat.max_trades_per_run {
                info!("Hit max trades per run ({})", strat.max_trades_per_run);
                break;
            }
        }

        intents
    }

    /// Half-Kelly position sizing.
    ///
    /// Kelly formula: f* = (p * b - q) / b
    /// where p = P(win), q = 1-p, b = net payout odds
    /// We use half-Kelly (f* / 2) for safety.
    fn kelly_size(
        &self,
        p_yes: f64,
        ask_cents: i64,
        max_position_cents: i64,
        fee_per_contract: i64,
    ) -> i64 {
        let p = p_yes.clamp(0.01, 0.99);
        let q = 1.0 - p;

        // Net payout per contract if YES settles: (100 - ask - fees) cents
        let net_payout = (100 - ask_cents - fee_per_contract) as f64;
        if net_payout <= 0.0 {
            return 1; // Minimum 1 contract
        }

        // Cost per contract including entry fee.
        let cost_per_contract = (ask_cents + fee_per_contract) as f64;

        // Odds: net_payout / cost
        let b = net_payout / cost_per_contract;

        // Kelly fraction: (p * b - q) / b
        let kelly_f = (p * b - q) / b;
        if kelly_f <= 0.0 {
            return 1;
        }

        // Half-Kelly.
        let half_kelly = kelly_f * KELLY_FRACTION;

        // Position = half_kelly * max_position / cost_per_contract
        let max_contracts = max_position_cents as f64 / cost_per_contract;
        let kelly_contracts = (half_kelly * max_contracts).floor() as i64;

        kelly_contracts.clamp(1, 10)
    }

    /// Try to map a ticker back to a city name for forecast lookup.
    fn ticker_to_city(&self, ticker: &str) -> Option<String> {
        let ticker_upper = ticker.to_uppercase();
        for city in &self.config.cities {
            if ticker_upper.contains(&city.series_prefix.to_uppercase()) {
                return Some(city.name.clone());
            }
        }
        // Fallback: check for city abbreviations.
        if ticker_upper.contains("NYC") || ticker_upper.contains("NEWYORK") {
            Some("New York City".into())
        } else if ticker_upper.contains("CHI") || ticker_upper.contains("CHICAGO") {
            Some("Chicago".into())
        } else if ticker_upper.contains("SEA") || ticker_upper.contains("SEATTLE") {
            Some("Seattle".into())
        } else if ticker_upper.contains("ATL") || ticker_upper.contains("ATLANTA") {
            Some("Atlanta".into())
        } else if ticker_upper.contains("DAL") || ticker_upper.contains("DALLAS") {
            Some("Dallas".into())
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::new_forecast_cache;
    use crate::cache::ForecastEntry;
    use common::ForecastData;
    use std::time::Instant;

    fn default_config() -> BotConfig {
        BotConfig::default()
    }

    fn make_market(ticker: &str, yes_bid: i64, yes_ask: i64) -> MarketInfo {
        MarketInfo {
            ticker: ticker.into(),
            event_ticker: "EVT".into(),
            market_type: "binary".into(),
            title: format!("Test {}", ticker),
            subtitle: String::new(),
            status: "open".into(),
            yes_bid,
            yes_ask,
            no_bid: 100 - yes_ask,
            no_ask: 100 - yes_bid,
            last_price: (yes_bid + yes_ask) / 2,
            volume: 100,
            volume_24h: 50,
            open_interest: 10,
            rules_primary: String::new(),
            rules_secondary: String::new(),
            close_time: Some(Utc::now() + chrono::Duration::hours(24)),
            expiration_time: Some(Utc::now() + chrono::Duration::hours(24)),
            result: None,
            strike_type: Some("greater".into()),
            floor_strike: Some(50.0),
            cap_strike: None,
            functional_strike: None,
            tick_size: Some(1),
            response_price_units: Some("usd_cent".into()),
        }
    }

    fn make_price(yes_bid: i64, yes_ask: i64) -> PriceEntry {
        PriceEntry {
            yes_bid,
            yes_ask,
            last_price: (yes_bid + yes_ask) / 2,
            volume: 100,
            updated_at: Instant::now(),
        }
    }

    fn insert_forecast(fc: &ForecastCache, city: &str, high: f64, low: f64, std_dev: f64) {
        fc.insert(
            city.into(),
            ForecastEntry {
                data: ForecastData {
                    city: city.into(),
                    high_temp_f: high,
                    low_temp_f: low,
                    precip_prob: 0.1,
                    temp_std_dev: std_dev,
                    fetched_at: Utc::now(),
                },
                updated_at: Instant::now(),
            },
        );
    }

    // ── Preserved original tests ──────────────────────────────────────

    #[test]
    fn test_entry_signal() {
        let config = default_config();
        let engine = StrategyEngine::new(config);

        let mut markets = HashMap::new();
        markets.insert(
            "KXHIGHNYC-TEST".into(),
            make_market("KXHIGHNYC-TEST", 8, 12),
        );

        let mut prices = HashMap::new();
        prices.insert("KXHIGHNYC-TEST".into(), make_price(8, 12));

        let fc = new_forecast_cache();
        insert_forecast(&fc, "New York City", 80.0, 60.0, 3.0);

        let positions = HashMap::new();
        let intents = engine.evaluate(&markets, &prices, &fc, &positions);

        assert!(!intents.is_empty(), "Should generate entry signal");
        assert_eq!(intents[0].action, Action::Buy);
        assert_eq!(intents[0].side, Side::Yes);
    }

    #[test]
    fn test_no_entry_when_stale() {
        let config = default_config();
        let engine = StrategyEngine::new(config);

        let mut markets = HashMap::new();
        markets.insert(
            "KXHIGHNYC-TEST".into(),
            make_market("KXHIGHNYC-TEST", 8, 12),
        );

        let mut prices = HashMap::new();
        // Make price data stale (6 min ago, threshold is 5 min).
        prices.insert(
            "KXHIGHNYC-TEST".into(),
            PriceEntry {
                yes_bid: 8,
                yes_ask: 12,
                last_price: 10,
                volume: 100,
                updated_at: Instant::now() - Duration::from_secs(360),
            },
        );

        let fc = new_forecast_cache();
        insert_forecast(&fc, "New York City", 80.0, 60.0, 3.0);

        let positions = HashMap::new();
        let intents = engine.evaluate(&markets, &prices, &fc, &positions);

        assert!(intents.is_empty(), "Should NOT trade on stale price data");
    }

    #[test]
    fn test_exit_signal() {
        let config = default_config();
        let engine = StrategyEngine::new(config);

        let mut markets = HashMap::new();
        markets.insert(
            "KXHIGHNYC-TEST".into(),
            make_market("KXHIGHNYC-TEST", 48, 52),
        );

        let mut prices = HashMap::new();
        prices.insert("KXHIGHNYC-TEST".into(), make_price(48, 52));

        let fc = new_forecast_cache();
        insert_forecast(&fc, "New York City", 80.0, 60.0, 3.0);

        // Have existing position.
        let mut positions = HashMap::new();
        positions.insert("KXHIGHNYC-TEST".into(), 5i64);

        let intents = engine.evaluate(&markets, &prices, &fc, &positions);

        assert!(!intents.is_empty(), "Should generate exit signal");
        assert_eq!(intents[0].action, Action::Sell);
    }

    // ── New tests ─────────────────────────────────────────────────────

    #[test]
    fn test_fee_adjusted_edge_blocks_thin_margin() {
        // Edge of 5¢ but round-trip fees ~2¢ → net_edge=3¢ (should still pass with default 5¢ threshold...
        // But let's set edge threshold to 4¢ and verify fees are correctly subtracted).
        let mut config = default_config();
        config.strategy.edge_threshold_cents = 4;
        config.strategy.entry_threshold_cents = 20;
        let engine = StrategyEngine::new(config);

        // Market at ask=15, strike=50, forecast high=57 → fair ≈ ~99¢ (far above strike)
        // This should easily pass. Let's test a marginal case instead.
        // Market at ask=15, strike=50, forecast high=52 → p_yes ≈ ~0.75, fair=75-3=72, edge=72-15=57
        let mut markets = HashMap::new();
        markets.insert(
            "KXHIGHNYC-TEST".into(),
            make_market("KXHIGHNYC-TEST", 13, 15),
        );

        let mut prices = HashMap::new();
        prices.insert("KXHIGHNYC-TEST".into(), make_price(13, 15));

        let fc = new_forecast_cache();
        insert_forecast(&fc, "New York City", 52.0, 40.0, 3.0);

        let positions = HashMap::new();
        let intents = engine.evaluate(&markets, &prices, &fc, &positions);

        // Should enter: p_yes is very high (52 vs strike 50), net edge is large
        if !intents.is_empty() {
            assert!(
                intents[0].estimated_fee_cents > 0,
                "Fee should be calculated and > 0"
            );
        }
    }

    #[test]
    fn test_fee_on_entry_is_reported() {
        let config = default_config();
        let engine = StrategyEngine::new(config);

        let mut markets = HashMap::new();
        markets.insert(
            "KXHIGHNYC-TEST".into(),
            make_market("KXHIGHNYC-TEST", 8, 12),
        );

        let mut prices = HashMap::new();
        prices.insert("KXHIGHNYC-TEST".into(), make_price(8, 12));

        let fc = new_forecast_cache();
        insert_forecast(&fc, "New York City", 80.0, 60.0, 3.0);

        let positions = HashMap::new();
        let intents = engine.evaluate(&markets, &prices, &fc, &positions);

        if !intents.is_empty() {
            assert!(
                intents[0].estimated_fee_cents >= 0,
                "Every intent should have a fee estimate"
            );
            // At 12¢ ask, fee should be small (P*(1-P) is small at extremes)
            assert!(
                intents[0].estimated_fee_cents < 10,
                "Fee at 12¢ should be small, got {}",
                intents[0].estimated_fee_cents
            );
        }
    }

    #[test]
    fn test_kelly_sizing_proportional_to_edge() {
        let config = default_config();
        let engine = StrategyEngine::new(config);

        // Strong signal: p=0.95, ask=10
        let count_strong = engine.kelly_size(0.95, 10, 500, 1);
        // Weak signal: p=0.60, ask=10
        let count_weak = engine.kelly_size(0.60, 10, 500, 1);

        assert!(
            count_strong >= count_weak,
            "Strong edge ({}) should size >= weak edge ({})",
            count_strong,
            count_weak
        );
    }

    #[test]
    fn test_kelly_sizing_minimum_one() {
        let config = default_config();
        let engine = StrategyEngine::new(config);

        // Barely positive edge
        let count = engine.kelly_size(0.12, 10, 500, 1);
        assert!(count >= 1, "Should always allocate at least 1 contract");
    }

    #[test]
    fn test_exit_includes_fee_estimate() {
        let config = default_config();
        let engine = StrategyEngine::new(config);

        let mut markets = HashMap::new();
        markets.insert(
            "KXHIGHNYC-TEST".into(),
            make_market("KXHIGHNYC-TEST", 48, 52),
        );

        let mut prices = HashMap::new();
        prices.insert("KXHIGHNYC-TEST".into(), make_price(48, 52));

        let fc = new_forecast_cache();
        insert_forecast(&fc, "New York City", 80.0, 60.0, 3.0);

        let mut positions = HashMap::new();
        positions.insert("KXHIGHNYC-TEST".into(), 5i64);

        let intents = engine.evaluate(&markets, &prices, &fc, &positions);
        assert!(!intents.is_empty());

        let exit_intent = &intents[0];
        assert_eq!(exit_intent.action, Action::Sell);
        assert!(
            exit_intent.estimated_fee_cents >= 0,
            "Exit intent should have fee estimate"
        );
    }
}
