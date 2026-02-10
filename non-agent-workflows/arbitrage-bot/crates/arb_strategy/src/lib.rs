//! Arb strategy crate.
//!
//! Discovers arb opportunities across Kalshi binary-outcome markets
//! and provides evaluation, execution, and risk management.

pub mod arb;
pub mod config;
pub mod exec;
pub mod fees;
pub mod quotes;
pub mod risk;
pub mod universe;

pub use arb::{ArbDirection, ArbEvaluator, ArbLeg, ArbOpportunity};
pub use exec::ArbExecutor;
pub use fees::ArbFeeModel;
pub use quotes::QuoteBook;
pub use risk::ArbRiskManager;
pub use universe::{ArbGroup, EdgeCase, UniverseBuilder};
