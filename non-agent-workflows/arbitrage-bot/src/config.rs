//! Configuration loader — merges env vars, .env file, and config.toml.

use common::Error;
use serde::{Deserialize, Serialize};
use std::path::Path;

use arb_strategy::config::{ArbConfig, ArbRiskConfig, ArbTimingConfig, ExecutionConfig};

// ── Arb-specific config types ─────────────────────────────────────────

/// Top-level arbitrage bot configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArbBotConfig {
    /// Kalshi API key ID.
    #[serde(default)]
    pub api_key: String,

    /// RSA private key PEM.
    #[serde(default)]
    pub secret_key: String,

    /// Use demo environment.
    #[serde(default = "default_true")]
    pub use_demo: bool,

    /// Arb strategy parameters.
    #[serde(default)]
    pub arb: ArbConfig,

    /// Timing parameters.
    #[serde(default)]
    pub timing: ArbTimingConfig,

    /// Risk management parameters.
    #[serde(default)]
    pub risk: ArbRiskConfig,

    /// Execution parameters.
    #[serde(default)]
    pub execution: ExecutionConfig,
}

fn default_true() -> bool {
    true
}

impl Default for ArbBotConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            secret_key: String::new(),
            use_demo: true,
            arb: ArbConfig::default(),
            timing: ArbTimingConfig::default(),
            risk: ArbRiskConfig::default(),
            execution: ExecutionConfig::default(),
        }
    }
}

// ── Config loader ─────────────────────────────────────────────────────

/// Load bot configuration from environment and optional config file.
pub fn load_config() -> Result<ArbBotConfig, Error> {
    // 1. Load .env file from project root or parent directories.
    if let Err(e) = dotenvy::dotenv() {
        tracing::debug!("No .env file loaded: {}", e);
    }

    // 2. Start with defaults.
    let mut config = ArbBotConfig::default();

    // 3. Try loading config.toml if it exists.
    let config_path = Path::new("config.toml");
    if config_path.exists() {
        let contents = std::fs::read_to_string(config_path)
            .map_err(|e| Error::Config(format!("Failed to read config.toml: {}", e)))?;
        config = toml::from_str(&contents)
            .map_err(|e| Error::Config(format!("Failed to parse config.toml: {}", e)))?;
    }

    // 4. Override with environment variables (highest priority).
    if let Ok(key) = std::env::var("KALSHI_API_KEY") {
        config.api_key = key;
    }
    if let Ok(secret) = std::env::var("KALSHI_SECRET_KEY") {
        config.secret_key = secret;
    }
    if let Ok(demo) = std::env::var("USE_DEMO") {
        config.use_demo = demo != "0" && demo.to_lowercase() != "false";
    }

    // 5. Validate required fields.
    if config.api_key.is_empty() {
        return Err(Error::Config(
            "KALSHI_API_KEY is required (set in .env or environment)".into(),
        ));
    }
    if config.secret_key.is_empty() {
        return Err(Error::Config(
            "KALSHI_SECRET_KEY is required (set in .env or environment)".into(),
        ));
    }

    Ok(config)
}
