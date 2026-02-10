//! Strategy evaluation engine.
//!
//! Evaluates all tracked markets on each tick, comparing forecast-derived
//! fair value against live market prices. Emits `OrderIntent`s for the
//! execution layer to process.

use std::collections::HashMap;
use std::time::Duration;

use chrono::Utc;
use common::config::BotConfig;
use common::{Action, MarketInfo, OrderIntent, Side};
use kalshi_client::PriceEntry;
use noaa_client::probability::compute_p_yes;
use tracing::{debug, info, warn};

use crate::cache::{ForecastCache, ForecastEntry};

/// The strategy engine that evaluates markets.
pub struct StrategyEngine {
    pub config: BotConfig,
}

impl StrategyEngine {
    pub fn new(config: BotConfig) -> Self {
        Self { config }
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
                let hours_left =
                    (close_time - now).num_minutes() as f64 / 60.0;
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

            if price.updated_at.elapsed() > Duration::from_secs(self.config.timing.price_stale_secs) {
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

            // 6. Compute fair value.
            let strike_type = market.strike_type.as_deref().unwrap_or("greater");
            let p_yes = compute_p_yes(
                forecast,
                strike_type,
                market.floor_strike,
                market.cap_strike,
            );

            let fair_cents = (100.0 * p_yes).floor() as i64 - strat.safety_margin_cents;
            let edge = fair_cents - price.yes_ask;

            debug!(
                "{}: p_yes={:.3}, fair={}¢, ask={}¢, bid={}¢, edge={}¢",
                ticker, p_yes, fair_cents, price.yes_ask, price.yes_bid, edge
            );

            let current_pos = positions.get(ticker).copied().unwrap_or(0);

            // 7. Entry signal — BUY YES.
            if price.yes_ask <= strat.entry_threshold_cents
                && edge >= strat.edge_threshold_cents
                && current_pos == 0
            {
                // Calculate position size (limit to max_position).
                let max_contracts = strat.max_position_cents / price.yes_ask;
                let count = max_contracts.max(1).min(10);

                info!(
                    "ENTRY: {} — BUY YES @ {}¢ x{} (fair={}¢, edge={}¢, p_yes={:.3})",
                    ticker, price.yes_ask, count, fair_cents, edge, p_yes
                );

                intents.push(OrderIntent {
                    ticker: ticker.clone(),
                    side: Side::Yes,
                    action: Action::Buy,
                    price_cents: price.yes_ask,
                    count,
                    reason: format!(
                        "entry: ask={}¢ < thresh={}¢, edge={}¢, p_yes={:.3}",
                        price.yes_ask, strat.entry_threshold_cents, edge, p_yes
                    ),
                });
            }

            // 8. Exit signal — SELL YES.
            if price.yes_bid >= strat.exit_threshold_cents && current_pos > 0 {
                info!(
                    "EXIT: {} — SELL YES @ {}¢ x{} (bid={}¢ >= thresh={}¢)",
                    ticker, price.yes_bid, current_pos, price.yes_bid, strat.exit_threshold_cents
                );

                intents.push(OrderIntent {
                    ticker: ticker.clone(),
                    side: Side::Yes,
                    action: Action::Sell,
                    price_cents: price.yes_bid,
                    count: current_pos,
                    reason: format!(
                        "exit: bid={}¢ >= thresh={}¢",
                        price.yes_bid, strat.exit_threshold_cents
                    ),
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
            floor_strike: Some(50),
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

    #[test]
    fn test_entry_signal() {
        let config = default_config();
        let engine = StrategyEngine::new(config);

        let mut markets = HashMap::new();
        markets.insert("KXHIGHNYC-TEST".into(), make_market("KXHIGHNYC-TEST", 8, 12));

        let mut prices = HashMap::new();
        prices.insert("KXHIGHNYC-TEST".into(), make_price(8, 12));

        let fc = new_forecast_cache();
        fc.insert("New York City".into(), ForecastEntry {
            data: ForecastData {
                city: "New York City".into(),
                high_temp_f: 80.0,  // Well above strike of 50
                low_temp_f: 60.0,
                precip_prob: 0.1,
                temp_std_dev: 3.0,
                fetched_at: Utc::now(),
            },
            updated_at: Instant::now(),
        });

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
        markets.insert("KXHIGHNYC-TEST".into(), make_market("KXHIGHNYC-TEST", 8, 12));

        let mut prices = HashMap::new();
        // Make price data stale (6 min ago, threshold is 5 min).
        prices.insert("KXHIGHNYC-TEST".into(), PriceEntry {
            yes_bid: 8,
            yes_ask: 12,
            last_price: 10,
            volume: 100,
            updated_at: Instant::now() - Duration::from_secs(360),
        });

        let fc = new_forecast_cache();
        fc.insert("New York City".into(), ForecastEntry {
            data: ForecastData {
                city: "New York City".into(),
                high_temp_f: 80.0,
                low_temp_f: 60.0,
                precip_prob: 0.1,
                temp_std_dev: 3.0,
                fetched_at: Utc::now(),
            },
            updated_at: Instant::now(),
        });

        let positions = HashMap::new();
        let intents = engine.evaluate(&markets, &prices, &fc, &positions);

        assert!(intents.is_empty(), "Should NOT trade on stale price data");
    }

    #[test]
    fn test_exit_signal() {
        let config = default_config();
        let engine = StrategyEngine::new(config);

        let mut markets = HashMap::new();
        markets.insert("KXHIGHNYC-TEST".into(), make_market("KXHIGHNYC-TEST", 48, 52));

        let mut prices = HashMap::new();
        prices.insert("KXHIGHNYC-TEST".into(), make_price(48, 52));

        let fc = new_forecast_cache();
        fc.insert("New York City".into(), ForecastEntry {
            data: ForecastData {
                city: "New York City".into(),
                high_temp_f: 80.0,
                low_temp_f: 60.0,
                precip_prob: 0.1,
                temp_std_dev: 3.0,
                fetched_at: Utc::now(),
            },
            updated_at: Instant::now(),
        });

        // Have existing position.
        let mut positions = HashMap::new();
        positions.insert("KXHIGHNYC-TEST".into(), 5i64);

        let intents = engine.evaluate(&markets, &prices, &fc, &positions);

        assert!(!intents.is_empty(), "Should generate exit signal");
        assert_eq!(intents[0].action, Action::Sell);
    }
}
