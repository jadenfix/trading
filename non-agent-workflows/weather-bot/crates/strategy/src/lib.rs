//! Strategy engine crate.
//!
//! Evaluates market opportunities and manages risk.

pub mod cache;
pub mod engine;
pub mod risk;

pub use cache::{new_forecast_cache, ForecastCache, ForecastEntry};
pub use engine::{EvaluationResult, RejectedSignal, StrategyEngine};
pub use risk::RiskManager;
