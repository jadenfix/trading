//! Exchange abstraction layer for normalized, venue-agnostic execution.

use serde::{Deserialize, Serialize};
use std::future::Future;
use std::pin::Pin;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum AssetClass {
    Crypto,
    Equity,
    Prediction,
    Fx,
    Rates,
    Commodity,
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum InstrumentType {
    Spot,
    Perpetual,
    Future,
    Option,
    BinaryOption,
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum OptionRight {
    Call,
    Put,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstrumentRef {
    pub venue: String,
    pub venue_symbol: String,
    pub asset_class: AssetClass,
    pub instrument_type: InstrumentType,
    pub base: Option<String>,
    pub quote: Option<String>,
    pub expiry_ts_ms: Option<i64>,
    pub strike: Option<f64>,
    pub option_right: Option<OptionRight>,
    pub contract_multiplier: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum OrderSide {
    Buy,
    Sell,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum OrderType {
    Limit,
    Market,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TimeInForce {
    Gtc,
    Ioc,
    Fok,
    Day,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum OrderStatus {
    New,
    PartiallyFilled,
    Filled,
    Canceled,
    Rejected,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizedOrderRequest {
    pub venue: String,
    pub symbol: String,
    pub instrument: InstrumentRef,
    pub strategy_id: String,
    pub client_order_id: String,
    pub intent_id: Option<String>,
    pub side: OrderSide,
    pub order_type: OrderType,
    pub qty: f64,
    pub limit_price: Option<f64>,
    pub tif: Option<TimeInForce>,
    pub post_only: bool,
    pub reduce_only: bool,
    pub requested_notional_cents: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderAck {
    pub venue_order_id: String,
    pub client_order_id: String,
    pub accepted: bool,
    pub status: OrderStatus,
    pub filled_qty: f64,
    pub avg_fill_price: Option<f64>,
    pub simulated: bool,
    pub reason: Option<String>,
    pub ts_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderSnapshot {
    pub venue: String,
    pub venue_order_id: String,
    pub client_order_id: String,
    pub strategy_id: String,
    pub instrument: InstrumentRef,
    pub side: OrderSide,
    pub order_type: OrderType,
    pub status: OrderStatus,
    pub qty: f64,
    pub filled_qty: f64,
    pub limit_price: Option<f64>,
    pub avg_fill_price: Option<f64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub simulated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenOrderSnapshot {
    pub order: OrderSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FillReport {
    pub venue: String,
    pub venue_fill_id: String,
    pub venue_order_id: String,
    pub client_order_id: String,
    pub strategy_id: String,
    pub instrument: InstrumentRef,
    pub side: OrderSide,
    pub qty: f64,
    pub price: f64,
    pub fee: f64,
    pub fee_asset: Option<String>,
    pub liquidity: Option<String>,
    pub simulated: bool,
    pub ts_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionSnapshot {
    pub venue: String,
    pub instrument: InstrumentRef,
    pub qty: f64,
    pub avg_price: f64,
    pub mark_price: Option<f64>,
    pub unrealized_pnl: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BalanceSnapshot {
    pub venue: String,
    pub asset: String,
    pub total: f64,
    pub available: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExchangeHealth {
    pub venue: String,
    pub healthy: bool,
    pub connected_market_data: bool,
    pub connected_trading: bool,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExchangeError {
    pub code: String,
    pub message: String,
    pub retriable: bool,
}

impl ExchangeError {
    pub fn new(code: impl Into<String>, message: impl Into<String>, retriable: bool) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retriable,
        }
    }
}

pub trait ExchangeAdapter: Send + Sync {
    fn venue(&self) -> &'static str;

    fn connect_market_data(&self) -> ExchangeResultFuture<'_, ()>;

    fn place_order(&self, req: NormalizedOrderRequest) -> ExchangeResultFuture<'_, OrderAck>;

    fn cancel_order(&self, venue_order_id: &str) -> ExchangeResultFuture<'_, ()>;

    fn get_order(&self, venue_order_id: &str) -> ExchangeResultFuture<'_, Option<OrderSnapshot>>;

    fn open_orders(&self) -> ExchangeResultFuture<'_, Vec<OpenOrderSnapshot>>;

    fn fills_since(
        &self,
        since_ts_ms: i64,
        limit: usize,
    ) -> ExchangeResultFuture<'_, Vec<FillReport>>;

    fn sync_positions(&self) -> ExchangeResultFuture<'_, Vec<PositionSnapshot>>;

    fn sync_balances(&self) -> ExchangeResultFuture<'_, Vec<BalanceSnapshot>>;

    fn health(&self) -> ExchangeValueFuture<'_, ExchangeHealth>;
}

pub type ExchangeResultFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ExchangeError>> + Send + 'a>>;
pub type ExchangeValueFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instrument_ref_preserves_asset_and_type() {
        let instrument = InstrumentRef {
            venue: "coinbase_at".to_string(),
            venue_symbol: "BTC-USD".to_string(),
            asset_class: AssetClass::Crypto,
            instrument_type: InstrumentType::Spot,
            base: Some("BTC".to_string()),
            quote: Some("USD".to_string()),
            expiry_ts_ms: None,
            strike: None,
            option_right: None,
            contract_multiplier: Some(1.0),
        };

        assert_eq!(instrument.asset_class, AssetClass::Crypto);
        assert_eq!(instrument.instrument_type, InstrumentType::Spot);
    }
}
