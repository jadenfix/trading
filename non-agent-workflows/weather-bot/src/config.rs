//! Configuration loader — merges env vars, .env file, and config.toml.

use common::config::{BotConfig, QualityMode};
use common::Error;
use std::path::Path;

fn parse_non_negative_f64(raw: &str, env_name: &str) -> Result<f64, Error> {
    let parsed = raw
        .trim()
        .parse::<f64>()
        .map_err(|_| Error::Config(format!("{env_name} must be a number >= 0")))?;
    if parsed < 0.0 {
        return Err(Error::Config(format!("{env_name} must be a number >= 0")));
    }
    Ok(parsed)
}

fn parse_non_negative_i64(raw: &str, env_name: &str) -> Result<i64, Error> {
    let parsed = raw
        .trim()
        .parse::<i64>()
        .map_err(|_| Error::Config(format!("{env_name} must be an integer >= 0")))?;
    if parsed < 0 {
        return Err(Error::Config(format!("{env_name} must be an integer >= 0")));
    }
    Ok(parsed)
}

fn parse_bool(raw: &str) -> bool {
    let lowered = raw.trim().to_ascii_lowercase();
    lowered != "0" && lowered != "false" && lowered != "no" && lowered != "off"
}

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
    if config.strategy.max_spread_cents < 0 {
        issues.push("strategy.max_spread_cents must be >= 0".into());
    }
    if config.strategy.min_hours_before_close < 0.0 {
        issues.push("strategy.min_hours_before_close must be >= 0".into());
    }
    if config.strategy.max_days_to_resolution <= 0 {
        issues.push("strategy.max_days_to_resolution must be > 0".into());
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

    if config.weather_sources.noaa_weight < 0.0 {
        issues.push("weather_sources.noaa_weight must be >= 0".into());
    }
    if config.weather_sources.google_weight < 0.0 {
        issues.push("weather_sources.google_weight must be >= 0".into());
    }
    if (config.weather_sources.noaa_weight + config.weather_sources.google_weight) <= 0.0 {
        issues.push("weather_sources total weight must be > 0".into());
    }

    if config.quality.max_source_prob_gap < 0.0 || config.quality.max_source_prob_gap > 1.0 {
        issues.push("quality.max_source_prob_gap must be in [0,1]".into());
    }
    if config.quality.min_source_confidence < 0.0 || config.quality.min_source_confidence > 1.0 {
        issues.push("quality.min_source_confidence must be in [0,1]".into());
    }
    if config.quality.min_ensemble_confidence < 0.0 || config.quality.min_ensemble_confidence > 1.0
    {
        issues.push("quality.min_ensemble_confidence must be in [0,1]".into());
    }
    if config.quality.min_conservative_net_edge_cents < 0 {
        issues.push("quality.min_conservative_net_edge_cents must be >= 0".into());
    }
    if config.quality.min_conservative_ev_cents < 0 {
        issues.push("quality.min_conservative_ev_cents must be >= 0".into());
    }
    if config.quality.min_volume_24h < 0 {
        issues.push("quality.min_volume_24h must be >= 0".into());
    }
    if config.quality.min_open_interest < 0 {
        issues.push("quality.min_open_interest must be >= 0".into());
    }
    if config.quality.slippage_buffer_cents < 0 {
        issues.push("quality.slippage_buffer_cents must be >= 0".into());
    }
    if config.quality.max_spread_cents_ultra < 0 {
        issues.push("quality.max_spread_cents_ultra must be >= 0".into());
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

fn apply_weather_source_fallbacks(config: &mut BotConfig) {
    let has_google_key = !config.google_weather_api_key.trim().is_empty();

    if !has_google_key {
        if config.weather_sources.google_weight > 0.0 {
            tracing::warn!(
                "GOOGLE_WEATHER_API_KEY is missing; disabling Google forecast weight and running NOAA-only"
            );
            config.weather_sources.google_weight = 0.0;
        }
        if config.quality.require_both_sources {
            tracing::warn!(
                "GOOGLE_WEATHER_API_KEY is missing; disabling quality.require_both_sources for graceful fallback"
            );
            config.quality.require_both_sources = false;
        }
    }

    if config.quality.require_both_sources
        && (config.weather_sources.noaa_weight <= 0.0
            || config.weather_sources.google_weight <= 0.0)
    {
        tracing::warn!(
            "quality.require_both_sources requires both NOAA and Google weights > 0; disabling this gate"
        );
        config.quality.require_both_sources = false;
    }

    if (config.weather_sources.noaa_weight + config.weather_sources.google_weight) <= 0.0 {
        tracing::warn!(
            "Both weather source weights are disabled; defaulting to NOAA-only (noaa_weight=1.0)"
        );
        config.weather_sources.noaa_weight = 1.0;
        config.weather_sources.google_weight = 0.0;
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
        config.use_demo = parse_bool(&demo);
    }
    if let Ok(days) = std::env::var("WEATHER_MAX_DAYS_TO_RESOLUTION") {
        let parsed = days.trim().parse::<i64>().map_err(|_| {
            Error::Config("WEATHER_MAX_DAYS_TO_RESOLUTION must be an integer > 0".into())
        })?;
        if parsed <= 0 {
            return Err(Error::Config(
                "WEATHER_MAX_DAYS_TO_RESOLUTION must be an integer > 0".into(),
            ));
        }
        config.strategy.max_days_to_resolution = parsed;
    }
    if let Ok(max_pos) = std::env::var("WEATHER_MAX_POSITION_CENTS") {
        let parsed = max_pos.trim().parse::<i64>().map_err(|_| {
            Error::Config("WEATHER_MAX_POSITION_CENTS must be an integer > 0".into())
        })?;
        if parsed <= 0 {
            return Err(Error::Config(
                "WEATHER_MAX_POSITION_CENTS must be an integer > 0".into(),
            ));
        }
        // Keep Kelly sizing logic, but enforce a lower cap for both strategy and risk.
        config.strategy.max_position_cents = parsed;
        config.risk.max_position_cents = parsed;
    }
    if let Ok(key) = std::env::var("GOOGLE_WEATHER_API_KEY") {
        config.google_weather_api_key = key;
    }
    if let Ok(weight) = std::env::var("WEATHER_NOAA_WEIGHT") {
        config.weather_sources.noaa_weight =
            parse_non_negative_f64(&weight, "WEATHER_NOAA_WEIGHT")?;
    }
    if let Ok(weight) = std::env::var("WEATHER_GOOGLE_WEIGHT") {
        config.weather_sources.google_weight =
            parse_non_negative_f64(&weight, "WEATHER_GOOGLE_WEIGHT")?;
    }
    if let Ok(mode) = std::env::var("WEATHER_QUALITY_MODE") {
        config.quality.mode = match mode.trim().to_ascii_lowercase().as_str() {
            "ultra_safe" | "ultrasafe" => QualityMode::UltraSafe,
            "balanced" => QualityMode::Balanced,
            "aggressive" => QualityMode::Aggressive,
            _ => {
                return Err(Error::Config(
                    "WEATHER_QUALITY_MODE must be one of: ultra_safe, balanced, aggressive".into(),
                ));
            }
        };
    }
    if let Ok(raw) = std::env::var("WEATHER_STRICT_SOURCE_VETO") {
        config.quality.strict_source_veto = parse_bool(&raw);
    }
    if let Ok(raw) = std::env::var("WEATHER_REQUIRE_BOTH_SOURCES") {
        config.quality.require_both_sources = parse_bool(&raw);
    }
    if let Ok(raw) = std::env::var("WEATHER_MAX_SOURCE_PROB_GAP") {
        config.quality.max_source_prob_gap =
            parse_non_negative_f64(&raw, "WEATHER_MAX_SOURCE_PROB_GAP")?;
    }
    if let Ok(raw) = std::env::var("WEATHER_MIN_SOURCE_CONFIDENCE") {
        config.quality.min_source_confidence =
            parse_non_negative_f64(&raw, "WEATHER_MIN_SOURCE_CONFIDENCE")?;
    }
    if let Ok(raw) = std::env::var("WEATHER_MIN_ENSEMBLE_CONFIDENCE") {
        config.quality.min_ensemble_confidence =
            parse_non_negative_f64(&raw, "WEATHER_MIN_ENSEMBLE_CONFIDENCE")?;
    }
    if let Ok(raw) = std::env::var("WEATHER_MIN_CONSERVATIVE_NET_EDGE_CENTS") {
        config.quality.min_conservative_net_edge_cents =
            parse_non_negative_i64(&raw, "WEATHER_MIN_CONSERVATIVE_NET_EDGE_CENTS")?;
    }
    if let Ok(raw) = std::env::var("WEATHER_MIN_CONSERVATIVE_EV_CENTS") {
        config.quality.min_conservative_ev_cents =
            parse_non_negative_i64(&raw, "WEATHER_MIN_CONSERVATIVE_EV_CENTS")?;
    }
    if let Ok(raw) = std::env::var("WEATHER_MIN_VOLUME_24H") {
        config.quality.min_volume_24h = parse_non_negative_i64(&raw, "WEATHER_MIN_VOLUME_24H")?;
    }
    if let Ok(raw) = std::env::var("WEATHER_MIN_OPEN_INTEREST") {
        config.quality.min_open_interest =
            parse_non_negative_i64(&raw, "WEATHER_MIN_OPEN_INTEREST")?;
    }
    if let Ok(raw) = std::env::var("WEATHER_SLIPPAGE_BUFFER_CENTS") {
        config.quality.slippage_buffer_cents =
            parse_non_negative_i64(&raw, "WEATHER_SLIPPAGE_BUFFER_CENTS")?;
    }
    if let Ok(raw) = std::env::var("WEATHER_MAX_SPREAD_CENTS_ULTRA") {
        config.quality.max_spread_cents_ultra =
            parse_non_negative_i64(&raw, "WEATHER_MAX_SPREAD_CENTS_ULTRA")?;
    }

    // 5. Graceful fallback logic for weather source config.
    apply_weather_source_fallbacks(&mut config);

    // 6. Validate required fields.
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

#[cfg(test)]
mod tests {
    use super::apply_weather_source_fallbacks;
    use common::config::BotConfig;

    #[test]
    fn missing_google_key_disables_google_requirements() {
        let mut cfg = BotConfig::default();
        cfg.weather_sources.noaa_weight = 0.5;
        cfg.weather_sources.google_weight = 0.5;
        cfg.quality.require_both_sources = true;
        cfg.google_weather_api_key.clear();

        apply_weather_source_fallbacks(&mut cfg);

        assert_eq!(cfg.weather_sources.google_weight, 0.0);
        assert!(!cfg.quality.require_both_sources);
        assert_eq!(cfg.weather_sources.noaa_weight, 0.5);
    }

    #[test]
    fn all_zero_weights_fallback_to_noaa_only() {
        let mut cfg = BotConfig::default();
        cfg.weather_sources.noaa_weight = 0.0;
        cfg.weather_sources.google_weight = 0.0;
        cfg.quality.require_both_sources = false;

        apply_weather_source_fallbacks(&mut cfg);

        assert_eq!(cfg.weather_sources.noaa_weight, 1.0);
        assert_eq!(cfg.weather_sources.google_weight, 0.0);
    }
}
