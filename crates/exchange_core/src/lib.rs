//! Exchange abstraction layer for normalized, venue-agnostic execution.

use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizedOrderRequest {
    pub venue: String,
    pub symbol: String,
    pub strategy_id: String,
    pub client_order_id: String,
    pub side: OrderSide,
    pub order_type: OrderType,
    pub qty: f64,
    pub limit_price: Option<f64>,
    pub tif: Option<TimeInForce>,
    pub post_only: bool,
    pub reduce_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderAck {
    pub venue_order_id: String,
    pub accepted: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionSnapshot {
    pub venue: String,
    pub symbol: String,
    pub qty: f64,
    pub avg_price: f64,
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

pub trait ExchangeAdapter: Send + Sync {
    fn venue(&self) -> &'static str;

    fn connect_market_data(&self) -> Result<(), ExchangeError>;

    fn place_order(&self, req: NormalizedOrderRequest) -> Result<OrderAck, ExchangeError>;

    fn cancel_order(&self, venue_order_id: &str) -> Result<(), ExchangeError>;

    fn sync_positions(&self) -> Result<Vec<PositionSnapshot>, ExchangeError>;

    fn sync_balances(&self) -> Result<Vec<BalanceSnapshot>, ExchangeError>;

    fn health(&self) -> ExchangeHealth;
}
