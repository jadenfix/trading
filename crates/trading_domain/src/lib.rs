//! Canonical multi-asset trading domain types shared across the trading engine.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AssetClass {
    Prediction,
    Crypto,
    Equity,
    Forex,
    Derivative,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MarketType {
    Spot,
    Binary,
    Perpetual,
    Futures,
    Option,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OptionType {
    Call,
    Put,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InstrumentId {
    pub venue_id: String,
    pub symbol: String,
    pub asset_class: AssetClass,
    pub market_type: MarketType,
    pub expiry_ts_ms: Option<i64>,
    pub strike: Option<f64>,
    pub option_type: Option<OptionType>,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

impl InstrumentId {
    pub fn key(&self) -> String {
        format!("{}:{}", self.venue_id, self.symbol)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OrderSide {
    Buy,
    Sell,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OrderType {
    Limit,
    Market,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TimeInForce {
    Gtc,
    Ioc,
    Fok,
    Day,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    Paper,
    Live,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OrderStatus {
    Pending,
    Accepted,
    Filled,
    Canceled,
    Rejected,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OrderRequest {
    pub venue_id: String,
    pub strategy_id: String,
    pub client_order_id: String,
    pub instrument: InstrumentId,
    pub side: OrderSide,
    pub order_type: OrderType,
    pub quantity: f64,
    pub limit_price: Option<f64>,
    pub tif: Option<TimeInForce>,
    pub post_only: bool,
    pub reduce_only: bool,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OrderAck {
    pub venue_order_id: String,
    pub accepted: bool,
    pub status: OrderStatus,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OrderSummary {
    pub venue_order_id: String,
    pub client_order_id: String,
    pub strategy_id: String,
    pub venue_id: String,
    pub instrument: InstrumentId,
    pub side: OrderSide,
    pub order_type: OrderType,
    pub quantity: f64,
    pub filled_quantity: f64,
    pub avg_fill_price: Option<f64>,
    pub status: OrderStatus,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PositionSnapshot {
    pub venue_id: String,
    pub instrument: InstrumentId,
    pub quantity: f64,
    pub avg_price: f64,
    pub mark_price: Option<f64>,
    pub unrealized_pnl: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BalanceSnapshot {
    pub venue_id: String,
    pub asset: String,
    pub total: f64,
    pub available: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VenueCapability {
    pub market_type: MarketType,
    pub supports_live: bool,
    pub supports_paper: bool,
    pub supports_post_only: bool,
    pub supports_reduce_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VenueHealth {
    pub venue_id: String,
    pub healthy: bool,
    pub connected_market_data: bool,
    pub connected_trading: bool,
    pub message: Option<String>,
}
