//! Domain types shared across the bot.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ── Kalshi Market Types ───────────────────────────────────────────────

/// A Kalshi market as returned by GET /trade-api/v2/markets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketInfo {
    pub ticker: String,
    pub event_ticker: String,
    #[serde(default)]
    pub market_type: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub subtitle: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub yes_bid: i64,
    #[serde(default)]
    pub yes_ask: i64,
    #[serde(default)]
    pub no_bid: i64,
    #[serde(default)]
    pub no_ask: i64,
    #[serde(default)]
    pub last_price: i64,
    #[serde(default)]
    pub volume: i64,
    #[serde(default)]
    pub volume_24h: i64,
    #[serde(default)]
    pub open_interest: i64,
    #[serde(default)]
    pub rules_primary: String,
    #[serde(default)]
    pub rules_secondary: String,
    #[serde(default)]
    pub close_time: Option<DateTime<Utc>>,
    #[serde(default)]
    pub expiration_time: Option<DateTime<Utc>>,
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default)]
    pub strike_type: Option<String>,
    #[serde(default)]
    pub floor_strike: Option<i64>,
    #[serde(default)]
    pub cap_strike: Option<i64>,
    #[serde(default)]
    pub functional_strike: Option<String>,
    #[serde(default)]
    pub tick_size: Option<i64>,
    #[serde(default)]
    pub response_price_units: Option<String>,
}

/// Paginated response from GET /trade-api/v2/markets.
#[derive(Debug, Clone, Deserialize)]
pub struct MarketsResponse {
    pub markets: Vec<MarketInfo>,
    #[serde(default)]
    pub cursor: Option<String>,
}

/// An order to be placed.
#[derive(Debug, Clone, Serialize)]
pub struct OrderIntent {
    /// Market ticker.
    pub ticker: String,
    /// "yes" or "no".
    pub side: Side,
    /// "buy" or "sell".
    pub action: Action,
    /// Limit price in cents (1-99).
    pub price_cents: i64,
    /// Number of contracts.
    pub count: i64,
    /// Reason for the trade (for logging).
    pub reason: String,
}

/// Order request body for Kalshi API.
#[derive(Debug, Clone, Serialize)]
pub struct CreateOrderRequest {
    pub ticker: String,
    pub side: Side,
    pub action: Action,
    pub client_order_id: String,
    pub count: i64,
    #[serde(rename = "type")]
    pub order_type: OrderType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub yes_price: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_price: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expiration_ts: Option<i64>,
}

/// Response from POST /trade-api/v2/portfolio/orders.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateOrderResponse {
    pub order: OrderInfo,
}

/// An order as returned by the Kalshi API.
#[derive(Debug, Clone, Deserialize)]
pub struct OrderInfo {
    pub order_id: String,
    #[serde(default)]
    pub client_order_id: String,
    pub ticker: String,
    pub side: Side,
    pub action: Action,
    #[serde(rename = "type")]
    pub order_type: OrderType,
    pub status: String,
    #[serde(default)]
    pub yes_price: i64,
    #[serde(default)]
    pub no_price: i64,
    #[serde(default)]
    pub fill_count: i64,
    #[serde(default)]
    pub remaining_count: i64,
    #[serde(default)]
    pub initial_count: i64,
    #[serde(default)]
    pub taker_fees: i64,
    #[serde(default)]
    pub maker_fees: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Yes,
    No,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Buy,
    Sell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderType {
    Limit,
    Market,
}

// ── Position Types ────────────────────────────────────────────────────

/// A position in a market.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub ticker: String,
    /// Number of YES contracts held (negative = short).
    #[serde(default)]
    pub yes_count: i64,
    /// Number of NO contracts held.
    #[serde(default)]
    pub no_count: i64,
    /// Total cost basis in cents.
    #[serde(default)]
    pub market_exposure: i64,
    /// Realized PnL in cents.
    #[serde(default)]
    pub realized_pnl: i64,
}

/// Portfolio positions response.
#[derive(Debug, Clone, Deserialize)]
pub struct PositionsResponse {
    #[serde(default)]
    pub market_positions: Vec<Position>,
    #[serde(default)]
    pub cursor: Option<String>,
}

/// Balance response.
#[derive(Debug, Clone, Deserialize)]
pub struct BalanceResponse {
    /// Balance in cents.
    pub balance: i64,
}

// ── Forecast Types ────────────────────────────────────────────────────

/// Processed forecast data for a city/date.
#[derive(Debug, Clone)]
pub struct ForecastData {
    /// City name.
    pub city: String,
    /// Forecast high temperature in °F.
    pub high_temp_f: f64,
    /// Forecast low temperature in °F.
    pub low_temp_f: f64,
    /// Precipitation probability (0.0 - 1.0).
    pub precip_prob: f64,
    /// Forecast uncertainty (std dev in °F).
    pub temp_std_dev: f64,
    /// When this forecast was fetched.
    pub fetched_at: DateTime<Utc>,
}

// ── WebSocket Types ───────────────────────────────────────────────────

/// A WebSocket subscribe command.
#[derive(Debug, Serialize)]
pub struct WsSubscribeCmd {
    pub id: u64,
    pub cmd: String,
    pub params: WsSubscribeParams,
}

#[derive(Debug, Serialize)]
pub struct WsSubscribeParams {
    pub channels: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub market_tickers: Option<Vec<String>>,
}

/// A ticker update message from the WebSocket.
#[derive(Debug, Clone, Deserialize)]
pub struct WsTickerMessage {
    #[serde(default)]
    pub market_ticker: String,
    #[serde(default)]
    pub yes_bid: i64,
    #[serde(default)]
    pub yes_ask: i64,
    #[serde(default)]
    pub last_price: i64,
    #[serde(default)]
    pub volume: i64,
}

/// Generic WebSocket message envelope.
#[derive(Debug, Clone, Deserialize)]
pub struct WsMessage {
    #[serde(rename = "type")]
    pub msg_type: Option<String>,
    pub msg: Option<serde_json::Value>,
    pub id: Option<u64>,
}
