//! Shared types, config, and error definitions for the weather-bot.

pub mod config;
pub mod error;
pub mod types;

pub use config::BotConfig;
pub use error::Error;
pub use types::*;

/// Convenience Result alias.
pub type Result<T> = std::result::Result<T, Error>;
