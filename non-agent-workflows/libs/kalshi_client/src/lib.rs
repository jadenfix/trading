//! Kalshi API client library.
//!
//! Provides authenticated REST and WebSocket access to the Kalshi trade API.

pub mod auth;
pub mod rate_limit;
pub mod rest;
pub mod ws;

pub use auth::KalshiAuth;
pub use rate_limit::RateLimiter;
pub use rest::KalshiRestClient;
pub use ws::{new_price_cache, KalshiWsClient, PriceCache, PriceEntry};
