//! Kalshi weather strategy plugin.

use exchange_core::{
    AssetClass, InstrumentRef, InstrumentType, NormalizedOrderRequest, OrderSide, OrderType,
    TimeInForce,
};
use strategy_core::{
    MarketRegime, RegimeContext, SignalIntent, StrategyError, StrategyFamily, StrategyPlugin,
};

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
            venue: "kalshi".to_string(),
            symbol: ctx.symbol.clone(),
            instrument: InstrumentRef {
                venue: "kalshi".to_string(),
                venue_symbol: ctx.symbol.clone(),
                asset_class: AssetClass::Prediction,
                instrument_type: InstrumentType::BinaryOption,
                base: None,
                quote: Some("USD".to_string()),
                expiry_ts_ms: None,
                strike: None,
                option_right: None,
                contract_multiplier: Some(1.0),
            },
            strategy_id: self.id().to_string(),
            client_order_id: format!("weather-{}", ctx.ts_ms),
            intent_id: Some(format!("{}-{}", self.id(), ctx.ts_ms)),
            side: OrderSide::Buy,
            order_type: OrderType::Limit,
            qty: 1.0,
            limit_price: Some(0.45),
            tif: Some(TimeInForce::Ioc),
            post_only: false,
            reduce_only: false,
            requested_notional_cents: 150,
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
