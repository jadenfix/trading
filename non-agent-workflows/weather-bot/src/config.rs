//! Configuration loader — merges env vars, .env file, and config.toml.

use common::config::BotConfig;
use common::Error;
use std::path::Path;

fn validate_config(config: &BotConfig) -> Result<(), Error> {
    let mut issues: Vec<String> = Vec::new();

    if config.cities.is_empty() {
        issues.push("cities must contain at least one city".into());
    }
    if config.series_prefixes.is_empty() {
        issues.push("series_prefixes must contain at least one prefix".into());
    }

    if config.strategy.entry_threshold_cents <= 0 {
        issues.push("strategy.entry_threshold_cents must be > 0".into());
    }
    if config.strategy.exit_threshold_cents <= 0 {
        issues.push("strategy.exit_threshold_cents must be > 0".into());
    }
    if config.strategy.edge_threshold_cents < 0 {
        issues.push("strategy.edge_threshold_cents must be >= 0".into());
    }
    if config.strategy.max_position_cents <= 0 {
        issues.push("strategy.max_position_cents must be > 0".into());
    }
    if config.strategy.max_trades_per_run == 0 {
        issues.push("strategy.max_trades_per_run must be > 0".into());
    }
    if config.strategy.max_spread_cents < 0 {
        issues.push("strategy.max_spread_cents must be >= 0".into());
    }
    if config.strategy.min_hours_before_close < 0.0 {
        issues.push("strategy.min_hours_before_close must be >= 0".into());
    }

    if config.risk.max_position_cents <= 0 {
        issues.push("risk.max_position_cents must be > 0".into());
    }
    if config.risk.max_total_exposure_cents <= 0 {
        issues.push("risk.max_total_exposure_cents must be > 0".into());
    }
    if config.risk.max_city_exposure_cents <= 0 {
        issues.push("risk.max_city_exposure_cents must be > 0".into());
    }
    if config.risk.max_daily_loss_cents <= 0 {
        issues.push("risk.max_daily_loss_cents must be > 0".into());
    }
    if config.risk.max_orders_per_minute == 0 {
        issues.push("risk.max_orders_per_minute must be > 0".into());
    }
    if config.risk.min_balance_cents < 0 {
        issues.push("risk.min_balance_cents must be >= 0".into());
    }
    if config.risk.max_total_exposure_cents < config.risk.max_position_cents {
        issues.push("risk.max_total_exposure_cents must be >= risk.max_position_cents".into());
    }
    if config.risk.max_total_exposure_cents < config.risk.max_city_exposure_cents {
        issues.push("risk.max_total_exposure_cents must be >= risk.max_city_exposure_cents".into());
    }

    if config.timing.scan_interval_secs == 0 {
        issues.push("timing.scan_interval_secs must be > 0".into());
    }
    if config.timing.discovery_interval_secs == 0 {
        issues.push("timing.discovery_interval_secs must be > 0".into());
    }
    if config.timing.forecast_interval_secs == 0 {
        issues.push("timing.forecast_interval_secs must be > 0".into());
    }
    if config.timing.price_stale_secs == 0 {
        issues.push("timing.price_stale_secs must be > 0".into());
    }
    if config.timing.forecast_stale_secs == 0 {
        issues.push("timing.forecast_stale_secs must be > 0".into());
    }

    if issues.is_empty() {
        Ok(())
    } else {
        Err(Error::Config(format!(
            "Invalid config:\n - {}",
            issues.join("\n - ")
        )))
    }
}

/// Load bot configuration from environment and optional config file.
pub fn load_config() -> Result<BotConfig, Error> {
    // 1. Load .env file from project root or parent directories.
    if let Err(e) = dotenvy::dotenv() {
        tracing::debug!("No .env file loaded: {}", e);
    }

    // 2. Start with defaults.
    let mut config = BotConfig::default();

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

    validate_config(&config)?;

    Ok(config)
}
