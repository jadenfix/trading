//! Strategy evaluation engine.
//!
//! Evaluates all tracked markets on each tick, comparing forecast-derived
//! fair value against live market prices. Emits `OrderIntent`s for the
//! execution layer to process.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::time::Duration;

use chrono::Utc;
use common::config::{BotConfig, QualityMode};
use common::{Action, FeeSchedule, MarketInfo, OrderIntent, Side};
use kalshi_client::PriceEntry;
use noaa_client::probability::compute_probability;
use tracing::{debug, info};

use crate::cache::{ForecastCache, ForecastEntry};

/// Safety floor for confidence filters.
const MIN_CONFIDENCE_FLOOR: f64 = 0.5;

/// A skipped opportunity with reason.
#[derive(Debug, Clone)]
pub struct RejectedSignal {
    pub ticker: String,
    pub reason: String,
}

/// Full strategy evaluation result.
#[derive(Debug, Default)]
pub struct EvaluationResult {
    pub intents: Vec<OrderIntent>,
    pub rejected: Vec<RejectedSignal>,
}

#[derive(Debug)]
struct EntryCandidate {
    intent: OrderIntent,
    score: f64,
}

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

    /// Backward-compatible evaluator returning only actionable intents.
    pub fn evaluate(
        &self,
        markets: &HashMap<String, MarketInfo>,
        prices: &HashMap<String, PriceEntry>,
        forecasts: &ForecastCache,
        positions: &HashMap<String, i64>,
    ) -> Vec<OrderIntent> {
        self.evaluate_with_diagnostics(markets, prices, forecasts, positions)
            .intents
    }

    /// Evaluate all markets and return intents with diagnostics.
    pub fn evaluate_with_diagnostics(
        &self,
        markets: &HashMap<String, MarketInfo>,
        prices: &HashMap<String, PriceEntry>,
        forecasts: &ForecastCache,
        positions: &HashMap<String, i64>,
    ) -> EvaluationResult {
        let mut rejected: Vec<RejectedSignal> = Vec::new();
        let mut exits: Vec<OrderIntent> = Vec::new();
        let mut entry_candidates: Vec<EntryCandidate> = Vec::new();

        let now = Utc::now();
        let strat = &self.config.strategy;
        let quality = &self.config.quality;
        let spread_cap = self.effective_spread_cap();

        for (ticker, market) in markets {
            if market.status != "open" {
                continue;
            }

            if let Some(close_time) = market.close_time {
                let hours_left = (close_time - now).num_minutes() as f64 / 60.0;
                if hours_left < strat.min_hours_before_close {
                    self.push_reject(
                        &mut rejected,
                        ticker,
                        format!("too_close_to_expiry:{hours_left:.2}h"),
                    );
                    continue;
                }
            }

            let price = match prices.get(ticker) {
                Some(p) => p,
                None => {
                    self.push_reject(&mut rejected, ticker, "no_price_data");
                    continue;
                }
            };

            if price.updated_at.elapsed() > Duration::from_secs(self.config.timing.price_stale_secs)
            {
                self.push_reject(&mut rejected, ticker, "stale_price_data");
                continue;
            }

            let spread = price.yes_ask - price.yes_bid;
            if spread > spread_cap {
                self.push_reject(
                    &mut rejected,
                    ticker,
                    format!("spread_filter:{spread}>{spread_cap}"),
                );
                continue;
            }

            let current_pos = positions.get(ticker).copied().unwrap_or(0);

            // Exit paths should always stay available, even under strict quality gating.
            if price.yes_bid >= strat.exit_threshold_cents && current_pos > 0 {
                let estimated_fee = self.fees.taker_fee_cents(current_pos, price.yes_bid);
                exits.push(OrderIntent {
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
                    confidence: 1.0,
                });
                continue;
            }

            // Entry-only liquidity filters.
            if market.volume_24h < quality.min_volume_24h
                || market.open_interest < quality.min_open_interest
            {
                self.push_reject(
                    &mut rejected,
                    ticker,
                    format!(
                        "liquidity_filter:vol24h={} oi={}",
                        market.volume_24h, market.open_interest
                    ),
                );
                continue;
            }

            let city_name = self.ticker_to_city(ticker);
            let forecast_entry: Option<ForecastEntry> = city_name
                .as_ref()
                .and_then(|name| forecasts.get(name).map(|e| e.clone()));

            let forecast = match forecast_entry {
                Some(ref entry) => {
                    if entry.is_stale(self.config.timing.forecast_stale_secs) {
                        self.push_reject(&mut rejected, ticker, "stale_forecast_data");
                        continue;
                    }
                    entry
                }
                None => {
                    self.push_reject(&mut rejected, ticker, "no_forecast_data");
                    continue;
                }
            };

            let strike_type = market.strike_type.as_deref().unwrap_or("greater");
            let ensemble_est = compute_probability(
                &forecast.ensemble,
                strike_type,
                market.floor_strike,
                market.cap_strike,
            );
            let noaa_est = forecast.noaa.as_ref().map(|fc| {
                compute_probability(fc, strike_type, market.floor_strike, market.cap_strike)
            });
            let google_est = forecast.google.as_ref().map(|fc| {
                compute_probability(fc, strike_type, market.floor_strike, market.cap_strike)
            });

            if quality.require_both_sources && (noaa_est.is_none() || google_est.is_none()) {
                self.push_reject(&mut rejected, ticker, "missing_source");
                continue;
            }

            // Strict veto: both sources required for disagreement check.
            if quality.strict_source_veto && (noaa_est.is_none() || google_est.is_none()) {
                self.push_reject(&mut rejected, ticker, "missing_source_for_strict_veto");
                continue;
            }

            let source_confidence_min = quality.min_source_confidence.max(MIN_CONFIDENCE_FLOOR);
            if let Some(est) = noaa_est {
                if est.confidence < source_confidence_min {
                    self.push_reject(
                        &mut rejected,
                        ticker,
                        format!("low_source_confidence:noaa:{:.3}", est.confidence),
                    );
                    continue;
                }
            }
            if let Some(est) = google_est {
                if est.confidence < source_confidence_min {
                    self.push_reject(
                        &mut rejected,
                        ticker,
                        format!("low_source_confidence:google:{:.3}", est.confidence),
                    );
                    continue;
                }
            }

            let ensemble_confidence_min = quality.min_ensemble_confidence.max(MIN_CONFIDENCE_FLOOR);
            if ensemble_est.confidence < ensemble_confidence_min {
                self.push_reject(
                    &mut rejected,
                    ticker,
                    format!("low_ensemble_confidence:{:.3}", ensemble_est.confidence),
                );
                continue;
            }

            let mut source_gap = 0.0;
            if let (Some(noaa), Some(google)) = (noaa_est, google_est) {
                source_gap = (noaa.p_yes - google.p_yes).abs();
                let noaa_dir_up = noaa.p_yes >= 0.5;
                let google_dir_up = google.p_yes >= 0.5;
                if quality.strict_source_veto {
                    if noaa_dir_up != google_dir_up {
                        self.push_reject(&mut rejected, ticker, "source_disagreement:direction");
                        continue;
                    }
                    if source_gap > quality.max_source_prob_gap {
                        self.push_reject(
                            &mut rejected,
                            ticker,
                            format!(
                                "source_disagreement:gap:{:.3}>{:.3}",
                                source_gap, quality.max_source_prob_gap
                            ),
                        );
                        continue;
                    }
                }
            }

            if price.yes_ask > strat.entry_threshold_cents {
                self.push_reject(
                    &mut rejected,
                    ticker,
                    format!("entry_threshold:ask={}¢", price.yes_ask),
                );
                continue;
            }

            if current_pos != 0 {
                self.push_reject(&mut rejected, ticker, "position_already_open");
                continue;
            }

            let mut conservative_candidates = vec![ensemble_est.p_yes_low];
            if let Some(est) = noaa_est {
                conservative_candidates.push(est.p_yes_low);
            }
            if let Some(est) = google_est {
                conservative_candidates.push(est.p_yes_low);
            }
            let p_conservative = conservative_candidates
                .into_iter()
                .fold(1.0, f64::min)
                .clamp(0.0, 1.0);

            let fair_cents =
                (100.0 * ensemble_est.p_yes).floor() as i64 - strat.safety_margin_cents;
            let conservative_fair =
                (100.0 * p_conservative).floor() as i64 - strat.safety_margin_cents;

            let entry_fee_per_contract = self.fees.per_contract_taker_fee(price.yes_ask);
            let exit_fee_per_contract =
                self.fees.per_contract_taker_fee(strat.exit_threshold_cents);
            let round_trip_fee_per_contract =
                (entry_fee_per_contract + exit_fee_per_contract).ceil() as i64;

            let slippage_buffer = quality.slippage_buffer_cents.max(0);
            let gross_edge = fair_cents - price.yes_ask;
            let net_edge = gross_edge - round_trip_fee_per_contract - slippage_buffer;
            let conservative_net_edge =
                conservative_fair - price.yes_ask - round_trip_fee_per_contract - slippage_buffer;
            let conservative_ev_cents = self.conservative_ev_cents(
                p_conservative,
                price.yes_ask,
                round_trip_fee_per_contract,
                slippage_buffer,
            );

            debug!(
                "{}: p_ens={:.3} [{:.3}–{:.3}] conf={:.2}, p_cons={:.3}, gap={:.3}, \
                 fair={}¢, cons_fair={}¢, ask={}¢, fees={}¢/c, net={}¢, cons_net={}¢, cons_ev={:.2}¢",
                ticker,
                ensemble_est.p_yes,
                ensemble_est.p_yes_low,
                ensemble_est.p_yes_high,
                ensemble_est.confidence,
                p_conservative,
                source_gap,
                fair_cents,
                conservative_fair,
                price.yes_ask,
                round_trip_fee_per_contract,
                net_edge,
                conservative_net_edge,
                conservative_ev_cents
            );

            if net_edge < strat.edge_threshold_cents {
                self.push_reject(
                    &mut rejected,
                    ticker,
                    format!(
                        "insufficient_edge:{}<{}",
                        net_edge, strat.edge_threshold_cents
                    ),
                );
                continue;
            }

            if conservative_net_edge < quality.min_conservative_net_edge_cents {
                self.push_reject(
                    &mut rejected,
                    ticker,
                    format!(
                        "insufficient_conservative_edge:{}<{}",
                        conservative_net_edge, quality.min_conservative_net_edge_cents
                    ),
                );
                continue;
            }

            if conservative_ev_cents < quality.min_conservative_ev_cents as f64 {
                self.push_reject(
                    &mut rejected,
                    ticker,
                    format!(
                        "insufficient_ev:{:.3}<{}",
                        conservative_ev_cents, quality.min_conservative_ev_cents
                    ),
                );
                continue;
            }

            let count = self.kelly_size(
                p_conservative,
                price.yes_ask,
                strat.max_position_cents,
                round_trip_fee_per_contract + slippage_buffer,
            );
            let estimated_fee = self.fees.taker_fee_cents(count, price.yes_ask);

            let reason = format!(
                "entry: ask={}¢, fair={}¢, net_edge={}¢, cons_edge={}¢, cons_ev={:.2}¢, p_cons={:.3}, conf={:.2}",
                price.yes_ask,
                fair_cents,
                net_edge,
                conservative_net_edge,
                conservative_ev_cents,
                p_conservative,
                ensemble_est.confidence
            );

            info!(
                "ENTRY: {} — BUY YES @ {}¢ x{} ({})",
                ticker, price.yes_ask, count, reason
            );

            let intent = OrderIntent {
                ticker: ticker.clone(),
                side: Side::Yes,
                action: Action::Buy,
                price_cents: price.yes_ask,
                count,
                reason,
                estimated_fee_cents: estimated_fee,
                confidence: ensemble_est.confidence,
            };

            let score = self.entry_quality_score(
                conservative_net_edge,
                conservative_ev_cents,
                source_gap,
                market.volume_24h,
                market.open_interest,
            );

            entry_candidates.push(EntryCandidate { intent, score });
        }

        entry_candidates.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal));

        let mut intents = exits;
        if strat.max_trades_per_run > 0 {
            let remaining = strat.max_trades_per_run;
            entry_candidates.truncate(remaining);
            info!(
                "Entry cap enabled: max_trades_per_run={} selected={}",
                strat.max_trades_per_run,
                entry_candidates.len()
            );
        } else {
            info!(
                "Entry cap disabled (max_trades_per_run=0): selected={}",
                entry_candidates.len()
            );
        }

        intents.extend(entry_candidates.into_iter().map(|c| c.intent));

        EvaluationResult { intents, rejected }
    }

    fn push_reject<S: Into<String>>(
        &self,
        rejected: &mut Vec<RejectedSignal>,
        ticker: &str,
        reason: S,
    ) {
        let reason = reason.into();
        debug!("{}: {}", ticker, reason);
        rejected.push(RejectedSignal {
            ticker: ticker.to_string(),
            reason,
        });
    }

    fn effective_spread_cap(&self) -> i64 {
        let strat_cap = self.config.strategy.max_spread_cents;
        let ultra_cap = self.config.quality.max_spread_cents_ultra;
        match self.config.quality.mode {
            QualityMode::UltraSafe => strat_cap.min(ultra_cap),
            QualityMode::Balanced | QualityMode::Aggressive => strat_cap,
        }
    }

    fn kelly_fraction(&self) -> f64 {
        match self.config.quality.mode {
            QualityMode::UltraSafe => 0.20,
            QualityMode::Balanced => 0.50,
            QualityMode::Aggressive => 0.75,
        }
    }

    fn conservative_ev_cents(
        &self,
        p_conservative: f64,
        ask_cents: i64,
        fee_per_contract: i64,
        slippage_buffer_cents: i64,
    ) -> f64 {
        let total_friction = fee_per_contract + slippage_buffer_cents;
        let win_payout = (100 - ask_cents - total_friction) as f64;
        let lose_cost = (ask_cents + total_friction) as f64;
        p_conservative * win_payout - (1.0 - p_conservative) * lose_cost
    }

    fn entry_quality_score(
        &self,
        conservative_net_edge: i64,
        conservative_ev_cents: f64,
        source_gap: f64,
        volume_24h: i64,
        open_interest: i64,
    ) -> f64 {
        let agreement_score = (1.0 - source_gap).clamp(0.0, 1.0) * 10.0;
        let liquidity_score =
            (volume_24h.min(500) as f64 / 500.0) + (open_interest.min(250) as f64 / 250.0);
        conservative_net_edge as f64 * 2.0
            + conservative_ev_cents
            + agreement_score
            + liquidity_score
    }

    /// Kelly-based position sizing.
    fn kelly_size(
        &self,
        p_yes: f64,
        ask_cents: i64,
        max_position_cents: i64,
        fee_per_contract: i64,
    ) -> i64 {
        let p = p_yes.clamp(0.01, 0.99);
        let q = 1.0 - p;

        let net_payout = (100 - ask_cents - fee_per_contract) as f64;
        if net_payout <= 0.0 {
            return 1;
        }

        let cost_per_contract = (ask_cents + fee_per_contract) as f64;
        let b = net_payout / cost_per_contract;

        let kelly_f = (p * b - q) / b;
        if kelly_f <= 0.0 {
            return 1;
        }

        let scaled_kelly = kelly_f * self.kelly_fraction();
        let max_contracts = max_position_cents as f64 / cost_per_contract;
        let kelly_contracts = (scaled_kelly * max_contracts).floor() as i64;

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
    use common::ForecastData;
    use std::time::Instant;

    fn default_config() -> BotConfig {
        let mut cfg = BotConfig::default();
        cfg.quality.mode = QualityMode::Balanced;
        cfg.quality.strict_source_veto = false;
        cfg.quality.require_both_sources = false;
        cfg.quality.max_source_prob_gap = 1.0;
        cfg.quality.min_source_confidence = 0.0;
        cfg.quality.min_ensemble_confidence = 0.0;
        cfg.quality.min_conservative_net_edge_cents = 0;
        cfg.quality.min_conservative_ev_cents = 0;
        cfg.quality.min_volume_24h = 0;
        cfg.quality.min_open_interest = 0;
        cfg.quality.slippage_buffer_cents = 0;
        cfg
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
            open_interest: 100,
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

    fn make_forecast(city: &str, high: f64, low: f64, std_dev: f64) -> ForecastData {
        ForecastData {
            city: city.into(),
            high_temp_f: high,
            low_temp_f: low,
            precip_prob: 0.1,
            temp_std_dev: std_dev,
            fetched_at: Utc::now(),
        }
    }

    fn insert_bundle(
        fc: &ForecastCache,
        city: &str,
        ensemble: ForecastData,
        noaa: Option<ForecastData>,
        google: Option<ForecastData>,
    ) {
        fc.insert(
            city.into(),
            ForecastEntry {
                ensemble,
                noaa,
                google,
                updated_at: Instant::now(),
            },
        );
    }

    fn insert_forecast(fc: &ForecastCache, city: &str, high: f64, low: f64, std_dev: f64) {
        let base = make_forecast(city, high, low, std_dev);
        insert_bundle(fc, city, base.clone(), Some(base.clone()), Some(base));
    }

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

        let mut positions = HashMap::new();
        positions.insert("KXHIGHNYC-TEST".into(), 5i64);

        let intents = engine.evaluate(&markets, &prices, &fc, &positions);

        assert!(!intents.is_empty(), "Should generate exit signal");
        assert_eq!(intents[0].action, Action::Sell);
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
            assert!(intents[0].estimated_fee_cents >= 0);
        }
    }

    #[test]
    fn test_kelly_sizing_proportional_to_edge() {
        let config = default_config();
        let engine = StrategyEngine::new(config);

        let count_strong = engine.kelly_size(0.95, 10, 500, 1);
        let count_weak = engine.kelly_size(0.60, 10, 500, 1);

        assert!(count_strong >= count_weak);
    }

    #[test]
    fn test_kelly_sizing_minimum_one() {
        let config = default_config();
        let engine = StrategyEngine::new(config);

        let count = engine.kelly_size(0.12, 10, 500, 1);
        assert!(count >= 1);
    }

    #[test]
    fn test_strict_veto_blocks_source_disagreement() {
        let mut config = default_config();
        config.quality.strict_source_veto = true;
        config.quality.require_both_sources = true;
        config.quality.max_source_prob_gap = 0.05;

        let engine = StrategyEngine::new(config);

        let mut markets = HashMap::new();
        markets.insert(
            "KXHIGHNYC-TEST".into(),
            make_market("KXHIGHNYC-TEST", 8, 12),
        );

        let mut prices = HashMap::new();
        prices.insert("KXHIGHNYC-TEST".into(), make_price(8, 12));

        let fc = new_forecast_cache();
        let ensemble = make_forecast("New York City", 60.0, 45.0, 2.0);
        let noaa = make_forecast("New York City", 92.0, 74.0, 2.0);
        let google = make_forecast("New York City", 32.0, 20.0, 2.0);
        insert_bundle(&fc, "New York City", ensemble, Some(noaa), Some(google));

        let positions = HashMap::new();
        let result = engine.evaluate_with_diagnostics(&markets, &prices, &fc, &positions);

        assert!(result.intents.is_empty());
        assert!(result
            .rejected
            .iter()
            .any(|r| r.reason.contains("source_disagreement")));
    }

    #[test]
    fn test_require_both_sources_blocks_missing_source() {
        let mut config = default_config();
        config.quality.strict_source_veto = true;
        config.quality.require_both_sources = true;

        let engine = StrategyEngine::new(config);

        let mut markets = HashMap::new();
        markets.insert(
            "KXHIGHNYC-TEST".into(),
            make_market("KXHIGHNYC-TEST", 8, 12),
        );

        let mut prices = HashMap::new();
        prices.insert("KXHIGHNYC-TEST".into(), make_price(8, 12));

        let fc = new_forecast_cache();
        let ensemble = make_forecast("New York City", 80.0, 60.0, 3.0);
        insert_bundle(
            &fc,
            "New York City",
            ensemble,
            Some(make_forecast("New York City", 80.0, 60.0, 3.0)),
            None,
        );

        let positions = HashMap::new();
        let result = engine.evaluate_with_diagnostics(&markets, &prices, &fc, &positions);

        assert!(result.intents.is_empty());
        assert!(result
            .rejected
            .iter()
            .any(|r| r.reason.contains("missing_source")));
    }

    #[test]
    fn test_exit_not_blocked_when_sources_missing() {
        let mut config = default_config();
        config.quality.strict_source_veto = true;
        config.quality.require_both_sources = true;

        let engine = StrategyEngine::new(config);

        let mut markets = HashMap::new();
        markets.insert(
            "KXHIGHNYC-TEST".into(),
            make_market("KXHIGHNYC-TEST", 48, 52),
        );

        let mut prices = HashMap::new();
        prices.insert("KXHIGHNYC-TEST".into(), make_price(48, 52));

        let fc = new_forecast_cache();
        let ensemble = make_forecast("New York City", 70.0, 55.0, 3.0);
        insert_bundle(&fc, "New York City", ensemble, None, None);

        let mut positions = HashMap::new();
        positions.insert("KXHIGHNYC-TEST".into(), 4);

        let intents = engine.evaluate(&markets, &prices, &fc, &positions);
        assert!(!intents.is_empty());
        assert_eq!(intents[0].action, Action::Sell);
    }

    #[test]
    fn test_max_trades_zero_means_unlimited() {
        let mut config = default_config();
        config.strategy.max_trades_per_run = 0;
        config.quality.min_conservative_net_edge_cents = 0;
        config.quality.min_conservative_ev_cents = 0;

        let engine = StrategyEngine::new(config);

        let mut markets = HashMap::new();
        markets.insert("KXHIGHNYC-A".into(), make_market("KXHIGHNYC-A", 8, 12));
        markets.insert("KXHIGHNYC-B".into(), make_market("KXHIGHNYC-B", 8, 12));

        let mut prices = HashMap::new();
        prices.insert("KXHIGHNYC-A".into(), make_price(8, 12));
        prices.insert("KXHIGHNYC-B".into(), make_price(8, 12));

        let fc = new_forecast_cache();
        insert_forecast(&fc, "New York City", 80.0, 60.0, 3.0);

        let positions = HashMap::new();
        let intents = engine.evaluate(&markets, &prices, &fc, &positions);

        let buys = intents.iter().filter(|i| i.action == Action::Buy).count();
        assert!(
            buys >= 2,
            "Expected unlimited buys with max_trades_per_run=0"
        );
    }
}
