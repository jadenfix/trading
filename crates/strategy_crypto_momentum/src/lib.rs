//! Crypto momentum/trend strategy plugin.

use exchange_core::{
    AssetClass, InstrumentRef, InstrumentType, NormalizedOrderRequest, OrderSide, OrderType,
};
use strategy_core::{
    MarketRegime, RegimeContext, SignalIntent, StrategyError, StrategyFamily, StrategyPlugin,
};

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
        let reduce_only = side == OrderSide::Sell;

        let instrument_type = if ctx.venue.eq_ignore_ascii_case("derivatives_paper") {
            InstrumentType::Perpetual
        } else {
            InstrumentType::Spot
        };

        let (base, quote) = ctx
            .symbol
            .split_once('-')
            .map(|(b, q)| (Some(b.to_string()), Some(q.to_string())))
            .unwrap_or_else(|| (Some(ctx.symbol.clone()), Some("USD".to_string())));

        let order = NormalizedOrderRequest {
            venue: ctx.venue.clone(),
            symbol: ctx.symbol.clone(),
            instrument: InstrumentRef {
                venue: ctx.venue.clone(),
                venue_symbol: ctx.symbol.clone(),
                asset_class: AssetClass::Crypto,
                instrument_type,
                base,
                quote,
                expiry_ts_ms: None,
                strike: None,
                option_right: None,
                contract_multiplier: Some(1.0),
            },
            strategy_id: self.id().to_string(),
            client_order_id: format!("mom-{}", ctx.ts_ms),
            intent_id: Some(format!("{}-{}", self.id(), ctx.ts_ms)),
            side,
            order_type: OrderType::Market,
            qty: 0.001,
            limit_price: None,
            tif: None,
            post_only: false,
            reduce_only,
            requested_notional_cents: 200,
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
