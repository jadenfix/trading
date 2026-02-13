//! Unified error type for the weather-bot.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("HTTP request failed: {0}")]
    Http(String),

    #[error("WebSocket error: {0}")]
    WebSocket(String),

    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Auth error: {0}")]
    Auth(String),

    #[error("Config error: {0}")]
    Config(String),

    #[error("NOAA API error: {0}")]
    Noaa(String),

    #[error("Google Weather API error: {0}")]
    GoogleWeather(String),

    #[error("Kalshi API error (status={status}): {message}")]
    KalshiApi { status: u16, message: String },

    #[error("Rate limited — retry after {retry_after_ms}ms")]
    RateLimited { retry_after_ms: u64 },

    #[error("Risk check failed: {0}")]
    RiskViolation(String),

    #[error("Market not found: {0}")]
    MarketNotFound(String),

    #[error("Stale data: {0}")]
    StaleData(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(String),
}
