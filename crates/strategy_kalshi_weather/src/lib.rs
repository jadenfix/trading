//! Kalshi weather strategy plugin.

use std::collections::BTreeMap;

use exchange_core::NormalizedOrderRequest;
use strategy_core::{
    MarketRegime, RegimeContext, SignalIntent, StrategyError, StrategyFamily, StrategyPlugin,
};
use trading_domain::{AssetClass, InstrumentId, MarketType, OrderSide, OrderType, TimeInForce};

#[derive(Debug, Default)]
pub struct KalshiWeatherStrategy;

impl StrategyPlugin for KalshiWeatherStrategy {
    fn id(&self) -> &'static str {
        "kalshi.market_making"
    }

    fn family(&self) -> StrategyFamily {
        StrategyFamily::MarketMaking
    }

    fn supports_venue(&self, venue: &str) -> bool {
        venue.eq_ignore_ascii_case("kalshi")
    }

    fn evaluate(&self, ctx: &RegimeContext) -> Result<Option<SignalIntent>, StrategyError> {
        if !self.supports_venue(&ctx.venue) {
            return Ok(None);
        }
        if ctx.regime != MarketRegime::EventDriven {
            return Ok(None);
        }
        if ctx.spread_bps > 120.0 {
            return Ok(None);
        }

        let order = NormalizedOrderRequest {
            venue_id: "kalshi".to_string(),
            strategy_id: self.id().to_string(),
            client_order_id: format!("weather-{}", ctx.ts_ms),
            instrument: InstrumentId {
                venue_id: "kalshi".to_string(),
                symbol: ctx.symbol.clone(),
                asset_class: AssetClass::Prediction,
                market_type: MarketType::Binary,
                expiry_ts_ms: None,
                strike: None,
                option_type: None,
                metadata: BTreeMap::new(),
            },
            side: OrderSide::Buy,
            order_type: OrderType::Limit,
            quantity: 1.0,
            limit_price: Some(0.45),
            tif: Some(TimeInForce::Ioc),
            post_only: false,
            reduce_only: false,
            metadata: BTreeMap::new(),
        };

        Ok(Some(SignalIntent {
            strategy_id: self.id().to_string(),
            family: self.family(),
            confidence: 0.72,
            horizon_ms: 60_000,
            expected_slippage_bps: 15.0,
            requested_risk_budget_cents: 150,
            order,
            rationale: "event-driven kalshi weather entry".to_string(),
        }))
    }
}
