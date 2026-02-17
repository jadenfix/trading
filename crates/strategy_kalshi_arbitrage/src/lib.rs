//! Kalshi arbitrage strategy plugin.

use exchange_core::{
    AssetClass, InstrumentRef, InstrumentType, NormalizedOrderRequest, OrderSide, OrderType,
    TimeInForce,
};
use strategy_core::{
    MarketRegime, RegimeContext, SignalIntent, StrategyError, StrategyFamily, StrategyPlugin,
};

#[derive(Debug, Default)]
pub struct KalshiArbitrageStrategy;

impl StrategyPlugin for KalshiArbitrageStrategy {
    fn id(&self) -> &'static str {
        "kalshi.arbitrage"
    }

    fn family(&self) -> StrategyFamily {
        StrategyFamily::Arbitrage
    }

    fn supports_venue(&self, venue: &str) -> bool {
        venue.eq_ignore_ascii_case("kalshi")
    }

    fn evaluate(&self, ctx: &RegimeContext) -> Result<Option<SignalIntent>, StrategyError> {
        if !self.supports_venue(&ctx.venue) {
            return Ok(None);
        }
        if !matches!(
            ctx.regime,
            MarketRegime::RangeBound | MarketRegime::LowVolatility
        ) {
            return Ok(None);
        }
        if ctx.spread_bps > 80.0 {
            return Ok(None);
        }

        let side = if ctx.order_book_imbalance >= 0.0 {
            OrderSide::Buy
        } else {
            OrderSide::Sell
        };
        let reduce_only = side == OrderSide::Sell;

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
            client_order_id: format!("arb-{}", ctx.ts_ms),
            intent_id: Some(format!("{}-{}", self.id(), ctx.ts_ms)),
            side,
            order_type: OrderType::Limit,
            qty: 1.0,
            limit_price: Some(0.50),
            tif: Some(TimeInForce::Fok),
            post_only: false,
            reduce_only,
            requested_notional_cents: 120,
        };

        Ok(Some(SignalIntent {
            strategy_id: self.id().to_string(),
            family: self.family(),
            confidence: 0.78,
            horizon_ms: 20_000,
            expected_slippage_bps: 8.0,
            requested_risk_budget_cents: 120,
            order,
            rationale: "short-horizon kalshi set-arb opportunity".to_string(),
        }))
    }
}
