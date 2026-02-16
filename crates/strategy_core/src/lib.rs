//! Strategy abstraction for regime-aware signal generation.

use exchange_core::NormalizedOrderRequest;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum StrategyFamily {
    Arbitrage,
    MarketMaking,
    MeanReversion,
    Momentum,
    VolatilityBreakout,
    Directional,
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MarketRegime {
    Trending,
    MeanReverting,
    HighVolatility,
    LowVolatility,
    RangeBound,
    EventDriven,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegimeContext {
    pub venue: String,
    pub symbol: String,
    pub regime: MarketRegime,
    pub spread_bps: f64,
    pub realized_volatility: f64,
    pub momentum_lookback_return: f64,
    pub order_book_imbalance: f64,
    pub ts_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalIntent {
    pub strategy_id: String,
    pub family: StrategyFamily,
    pub confidence: f64,
    pub horizon_ms: u64,
    pub expected_slippage_bps: f64,
    pub requested_risk_budget_cents: i64,
    pub order: NormalizedOrderRequest,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyError {
    pub code: String,
    pub message: String,
}

pub trait StrategyPlugin: Send + Sync {
    fn id(&self) -> &'static str;

    fn family(&self) -> StrategyFamily;

    fn supports_venue(&self, _venue: &str) -> bool {
        true
    }

    fn evaluate(&self, ctx: &RegimeContext) -> Result<Option<SignalIntent>, StrategyError>;
}
