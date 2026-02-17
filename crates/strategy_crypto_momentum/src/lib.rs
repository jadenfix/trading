//! Crypto momentum/trend strategy plugin.

use std::collections::BTreeMap;

use exchange_core::NormalizedOrderRequest;
use strategy_core::{
    MarketRegime, RegimeContext, SignalIntent, StrategyError, StrategyFamily, StrategyPlugin,
};
use trading_domain::{AssetClass, InstrumentId, MarketType, OrderSide, OrderType};

#[derive(Debug, Default)]
pub struct CryptoMomentumStrategy;

impl StrategyPlugin for CryptoMomentumStrategy {
    fn id(&self) -> &'static str {
        "crypto.momentum_trend"
    }

    fn family(&self) -> StrategyFamily {
        StrategyFamily::Momentum
    }

    fn supports_venue(&self, venue: &str) -> bool {
        venue.eq_ignore_ascii_case("coinbase_spot")
            || venue.eq_ignore_ascii_case("derivatives_paper")
    }

    fn evaluate(&self, ctx: &RegimeContext) -> Result<Option<SignalIntent>, StrategyError> {
        if !self.supports_venue(&ctx.venue) {
            return Ok(None);
        }
        if ctx.regime != MarketRegime::Trending {
            return Ok(None);
        }

        let side = if ctx.momentum_lookback_return >= 0.0 {
            OrderSide::Buy
        } else {
            OrderSide::Sell
        };

        let market_type = if ctx.venue.eq_ignore_ascii_case("derivatives_paper") {
            MarketType::Perpetual
        } else {
            MarketType::Spot
        };

        let order = NormalizedOrderRequest {
            venue_id: ctx.venue.clone(),
            strategy_id: self.id().to_string(),
            client_order_id: format!("mom-{}", ctx.ts_ms),
            instrument: InstrumentId {
                venue_id: ctx.venue.clone(),
                symbol: ctx.symbol.clone(),
                asset_class: AssetClass::Crypto,
                market_type,
                expiry_ts_ms: None,
                strike: None,
                option_type: None,
                metadata: BTreeMap::new(),
            },
            side,
            order_type: OrderType::Market,
            quantity: 0.001,
            limit_price: None,
            tif: None,
            post_only: false,
            reduce_only: side == OrderSide::Sell,
            metadata: BTreeMap::new(),
        };

        Ok(Some(SignalIntent {
            strategy_id: self.id().to_string(),
            family: self.family(),
            confidence: 0.70,
            horizon_ms: 30_000,
            expected_slippage_bps: 25.0,
            requested_risk_budget_cents: 200,
            order,
            rationale: "momentum/trend follow-on signal".to_string(),
        }))
    }
}
