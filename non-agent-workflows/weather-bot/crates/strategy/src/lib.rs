//! Strategy engine crate.
//!
//! Evaluates market opportunities and manages risk.

pub mod cache;
pub mod engine;
pub mod risk;

pub use cache::{ForecastCache, ForecastEntry, new_forecast_cache};
pub use engine::StrategyEngine;
pub use risk::RiskManager;
