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
    /// Estimated fee in cents for this order.
    #[serde(skip_serializing)]
    pub estimated_fee_cents: i64,
    /// Forecast confidence (0.0–1.0) at time of signal generation.
    #[serde(skip_serializing)]
    pub confidence: f64,
}

// ── Kalshi Fee Schedule ───────────────────────────────────────────────

/// Kalshi fee calculator (Feb 2026 schedule).
///
/// Taker formula: `ceil_to_cent(coeff × C × P × (1 − P))`
/// where P is price in dollars and C is contract count.
///
/// Maker formula: same but with ¼ the coefficient.
#[derive(Debug, Clone)]
pub struct FeeSchedule {
    /// Taker coefficient (0.07 for weather markets).
    pub taker_coeff: f64,
    /// Maker coefficient (0.0175 for weather markets).
    pub maker_coeff: f64,
}

impl FeeSchedule {
    /// Standard fee schedule for weather markets.
    pub fn weather() -> Self {
        Self {
            taker_coeff: 0.07,
            maker_coeff: 0.0175,
        }
    }

    /// Compute taker fee in cents (rounded up to next cent).
    pub fn taker_fee_cents(&self, count: i64, price_cents: i64) -> i64 {
        self.fee_cents(self.taker_coeff, count, price_cents)
    }

    /// Compute maker fee in cents (rounded up to next cent).
    pub fn maker_fee_cents(&self, count: i64, price_cents: i64) -> i64 {
        self.fee_cents(self.maker_coeff, count, price_cents)
    }

    /// Estimate round-trip cost in cents (taker entry + taker exit).
    ///
    /// Conservative: assumes taker on both legs.
    pub fn round_trip_fee_cents(&self, count: i64, entry_cents: i64, exit_cents: i64) -> i64 {
        self.taker_fee_cents(count, entry_cents) + self.taker_fee_cents(count, exit_cents)
    }

    /// Per-contract taker fee in fractional cents (for edge calculations).
    pub fn per_contract_taker_fee(&self, price_cents: i64) -> f64 {
        let p = price_cents as f64 / 100.0;
        self.taker_coeff * p * (1.0 - p) * 100.0 // result in cents
    }

    fn fee_cents(&self, coeff: f64, count: i64, price_cents: i64) -> i64 {
        let p = price_cents as f64 / 100.0;
        let fee_dollars = coeff * (count as f64) * p * (1.0 - p);
        // Round up to next cent. Use a small epsilon to avoid floating-point
        // precision issues (e.g., $1.75 being represented as $1.7500000000000002).
        let fee_cents = fee_dollars * 100.0;
        (fee_cents - 1e-9).ceil().max(0.0) as i64
    }
}

impl Default for FeeSchedule {
    fn default() -> Self {
        Self::weather()
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_taker_fee_50c_100_contracts() {
        // Kalshi docs: 100 contracts at 50¢ → $1.75
        let fs = FeeSchedule::weather();
        let fee = fs.taker_fee_cents(100, 50);
        assert_eq!(fee, 175, "100 contracts @ 50¢ should be 175¢ ($1.75)");
    }

    #[test]
    fn test_taker_fee_1c_100_contracts() {
        // Kalshi docs: 100 contracts at 1¢ → $0.07
        let fs = FeeSchedule::weather();
        let fee = fs.taker_fee_cents(100, 1);
        assert_eq!(fee, 7, "100 contracts @ 1¢ should be 7¢ ($0.07)");
    }

    #[test]
    fn test_taker_fee_99c_symmetric() {
        // P(1-P) is symmetric: fee at 99¢ == fee at 1¢
        let fs = FeeSchedule::weather();
        let fee_1 = fs.taker_fee_cents(100, 1);
        let fee_99 = fs.taker_fee_cents(100, 99);
        assert_eq!(fee_1, fee_99, "Fee should be symmetric around 50¢");
    }

    #[test]
    fn test_maker_fee_quarter_of_taker() {
        let fs = FeeSchedule::weather();
        // Maker coefficient is ¼ of taker
        assert!((fs.maker_coeff - fs.taker_coeff / 4.0).abs() < 1e-10);
    }

    #[test]
    fn test_round_trip_fee() {
        let fs = FeeSchedule::weather();
        let rt = fs.round_trip_fee_cents(10, 20, 45);
        let entry = fs.taker_fee_cents(10, 20);
        let exit = fs.taker_fee_cents(10, 45);
        assert_eq!(rt, entry + exit);
    }

    #[test]
    fn test_fee_ceil_rounding() {
        // Verify ceil behavior: even partial cents round up
        let fs = FeeSchedule::weather();
        // 1 contract at 50¢: 0.07 * 1 * 0.5 * 0.5 = $0.0175 → ceil to $0.02 = 2¢
        let fee = fs.taker_fee_cents(1, 50);
        assert_eq!(fee, 2, "Fractional cents should round up");
    }
}

