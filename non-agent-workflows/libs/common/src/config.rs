//! Bot configuration types.

use serde::{Deserialize, Serialize};

/// Top-level bot configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotConfig {
    /// Kalshi API key ID.
    #[serde(default)]
    pub api_key: String,

    /// RSA private key PEM (with literal \n for newlines).
    #[serde(default)]
    pub secret_key: String,

    /// Use demo environment (true) or production (false).
    #[serde(default = "default_true")]
    pub use_demo: bool,

    /// Cities to monitor.
    #[serde(default = "default_cities")]
    pub cities: Vec<CityConfig>,

    /// Weather series ticker prefixes to discover markets.
    #[serde(default = "default_series_prefixes")]
    pub series_prefixes: Vec<String>,

    /// Strategy parameters.
    #[serde(default)]
    pub strategy: StrategyConfig,

    /// Risk management parameters.
    #[serde(default)]
    pub risk: RiskConfig,

    /// Timing parameters (seconds).
    #[serde(default)]
    pub timing: TimingConfig,

    /// Forecast source blend settings.
    #[serde(default)]
    pub weather_sources: WeatherSourcesConfig,

    /// Google Weather API key (optional when google_weight is 0).
    #[serde(default)]
    pub google_weather_api_key: String,

    /// Trade quality controls for selective signal generation.
    #[serde(default)]
    pub quality: QualityConfig,
}

/// Configuration for a single city.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CityConfig {
    /// Human-readable name.
    pub name: String,
    /// Latitude.
    pub lat: f64,
    /// Longitude.
    pub lon: f64,
    /// NWS Weather Forecast Office ID (e.g., "OKX" for NYC).
    pub wfo: String,
    /// Grid X coordinate for the WFO.
    pub grid_x: u32,
    /// Grid Y coordinate for the WFO.
    pub grid_y: u32,
    /// Kalshi series ticker prefix (e.g., "KXHIGHNYC").
    pub series_prefix: String,
}

/// Strategy thresholds and limits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyConfig {
    /// Entry threshold in cents — buy YES when yes_ask <= this.
    #[serde(default = "default_entry")]
    pub entry_threshold_cents: i64,

    /// Exit threshold in cents — sell YES when yes_bid >= this.
    #[serde(default = "default_exit")]
    pub exit_threshold_cents: i64,

    /// Minimum edge (fair - ask) in cents to enter.
    #[serde(default = "default_edge")]
    pub edge_threshold_cents: i64,

    /// Safety margin subtracted from fair value (cents).
    #[serde(default = "default_safety")]
    pub safety_margin_cents: i64,

    /// Max position per market in cents (e.g., 500 = $5).
    #[serde(default = "default_max_position")]
    pub max_position_cents: i64,

    /// Max trades per strategy evaluation cycle.
    #[serde(default = "default_max_trades")]
    pub max_trades_per_run: usize,

    /// Max spread in cents — skip markets with wider spreads.
    #[serde(default = "default_max_spread")]
    pub max_spread_cents: i64,

    /// Minimum hours before market close to trade.
    #[serde(default = "default_min_hours")]
    pub min_hours_before_close: f64,

    /// Only include markets resolving in fewer than this many days.
    #[serde(default = "default_max_days_to_resolution")]
    pub max_days_to_resolution: i64,
}

/// Risk management thresholds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskConfig {
    /// Max position per market in cents (e.g., 500 = $5).
    #[serde(default = "default_max_position")]
    pub max_position_cents: i64,

    /// Max total portfolio exposure in cents.
    #[serde(default = "default_max_total_exposure")]
    pub max_total_exposure_cents: i64,

    /// Max exposure per city (across all correlated markets).
    #[serde(default = "default_max_city_exposure")]
    pub max_city_exposure_cents: i64,

    /// Max daily drawdown in cents before circuit-breaker halts buys.
    #[serde(default = "default_max_daily_loss")]
    pub max_daily_loss_cents: i64,

    /// Max orders per minute (sliding window).
    #[serde(default = "default_max_orders_per_min")]
    pub max_orders_per_minute: u32,

    /// Minimum balance to maintain (cents).
    #[serde(default = "default_min_balance")]
    pub min_balance_cents: i64,
}

/// Timing configuration (all values in seconds).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimingConfig {
    /// Strategy evaluation interval.
    #[serde(default = "default_scan_interval")]
    pub scan_interval_secs: u64,

    /// Forecast refresh interval.
    #[serde(default = "default_forecast_interval")]
    pub forecast_interval_secs: u64,

    /// Market discovery refresh interval.
    #[serde(default = "default_discovery_interval")]
    pub discovery_interval_secs: u64,

    /// Max age for price data before considered stale (seconds).
    #[serde(default = "default_price_stale")]
    pub price_stale_secs: u64,

    /// Max age for forecast data before considered stale (seconds).
    #[serde(default = "default_forecast_stale")]
    pub forecast_stale_secs: u64,
}

/// Forecast source weighting for ensemble forecasts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeatherSourcesConfig {
    /// Relative weight for NOAA source.
    #[serde(default = "default_noaa_weight")]
    pub noaa_weight: f64,

    /// Relative weight for Google Weather source.
    #[serde(default = "default_google_weight")]
    pub google_weight: f64,
}

/// Strategy quality modes.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QualityMode {
    UltraSafe,
    Balanced,
    Aggressive,
}

/// Signal quality thresholds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityConfig {
    /// Quality mode (affects spread and sizing policy).
    #[serde(default = "default_quality_mode")]
    pub mode: QualityMode,
    /// If true, source disagreement blocks entries.
    #[serde(default = "default_true")]
    pub strict_source_veto: bool,
    /// If true, both NOAA and Google estimates must be available for entries.
    #[serde(default = "default_true")]
    pub require_both_sources: bool,
    /// Maximum absolute probability gap between sources.
    #[serde(default = "default_max_source_prob_gap")]
    pub max_source_prob_gap: f64,
    /// Minimum source-specific confidence.
    #[serde(default = "default_min_source_confidence")]
    pub min_source_confidence: f64,
    /// Minimum ensemble confidence.
    #[serde(default = "default_min_ensemble_confidence")]
    pub min_ensemble_confidence: f64,
    /// Minimum conservative net edge in cents, after fees/slippage.
    #[serde(default = "default_min_conservative_net_edge_cents")]
    pub min_conservative_net_edge_cents: i64,
    /// Minimum conservative expected value in cents per contract.
    #[serde(default = "default_min_conservative_ev_cents")]
    pub min_conservative_ev_cents: i64,
    /// Minimum 24h volume.
    #[serde(default = "default_min_volume_24h")]
    pub min_volume_24h: i64,
    /// Minimum open interest.
    #[serde(default = "default_min_open_interest")]
    pub min_open_interest: i64,
    /// Extra slippage buffer in cents per contract.
    #[serde(default = "default_slippage_buffer_cents")]
    pub slippage_buffer_cents: i64,
    /// Ultra-safe spread cap in cents.
    #[serde(default = "default_max_spread_cents_ultra")]
    pub max_spread_cents_ultra: i64,
}

// ── Defaults ──────────────────────────────────────────────────────────

fn default_true() -> bool {
    true
}

fn default_entry() -> i64 {
    15
}
fn default_exit() -> i64 {
    45
}
fn default_edge() -> i64 {
    5
}
fn default_safety() -> i64 {
    3
}
fn default_max_position() -> i64 {
    500
}
fn default_max_trades() -> usize {
    5
}
fn default_max_spread() -> i64 {
    10
}
fn default_min_hours() -> f64 {
    2.0
}
fn default_max_days_to_resolution() -> i64 {
    11
}

fn default_max_total_exposure() -> i64 {
    5000
}
fn default_max_city_exposure() -> i64 {
    1500
}
fn default_max_daily_loss() -> i64 {
    2000
}
fn default_max_orders_per_min() -> u32 {
    10
}
fn default_min_balance() -> i64 {
    100
}

fn default_scan_interval() -> u64 {
    120
}
fn default_forecast_interval() -> u64 {
    1800
}
fn default_discovery_interval() -> u64 {
    1800
}
fn default_price_stale() -> u64 {
    300
}
fn default_forecast_stale() -> u64 {
    3600
}
fn default_noaa_weight() -> f64 {
    0.5
}
fn default_google_weight() -> f64 {
    0.5
}
fn default_quality_mode() -> QualityMode {
    QualityMode::UltraSafe
}
fn default_max_source_prob_gap() -> f64 {
    0.08
}
fn default_min_source_confidence() -> f64 {
    0.65
}
fn default_min_ensemble_confidence() -> f64 {
    0.75
}
fn default_min_conservative_net_edge_cents() -> i64 {
    8
}
fn default_min_conservative_ev_cents() -> i64 {
    4
}
fn default_min_volume_24h() -> i64 {
    50
}
fn default_min_open_interest() -> i64 {
    25
}
fn default_slippage_buffer_cents() -> i64 {
    1
}
fn default_max_spread_cents_ultra() -> i64 {
    6
}

fn default_cities() -> Vec<CityConfig> {
    vec![
        CityConfig {
            name: "New York City".into(),
            lat: 40.7128,
            lon: -74.0060,
            wfo: "OKX".into(),
            grid_x: 33,
            grid_y: 37,
            series_prefix: "KXHIGHNYC".into(),
        },
        CityConfig {
            name: "Chicago".into(),
            lat: 41.8781,
            lon: -87.6298,
            wfo: "LOT".into(),
            grid_x: 76,
            grid_y: 73,
            series_prefix: "KXHIGHCHI".into(),
        },
        CityConfig {
            name: "Seattle".into(),
            lat: 47.6062,
            lon: -122.3321,
            wfo: "SEW".into(),
            grid_x: 124,
            grid_y: 67,
            series_prefix: "KXHIGHSEA".into(),
        },
        CityConfig {
            name: "Atlanta".into(),
            lat: 33.7490,
            lon: -84.3880,
            wfo: "FFC".into(),
            grid_x: 50,
            grid_y: 86,
            series_prefix: "KXHIGHATL".into(),
        },
        CityConfig {
            name: "Dallas".into(),
            lat: 32.7767,
            lon: -96.7970,
            wfo: "FWD".into(),
            grid_x: 80,
            grid_y: 108,
            series_prefix: "KXHIGHDAL".into(),
        },
    ]
}

fn default_series_prefixes() -> Vec<String> {
    vec![
        "KXHIGHNYC".into(),
        "KXHIGHCHI".into(),
        "KXHIGHSEA".into(),
        "KXHIGHATL".into(),
        "KXHIGHDAL".into(),
    ]
}

impl Default for StrategyConfig {
    fn default() -> Self {
        Self {
            entry_threshold_cents: default_entry(),
            exit_threshold_cents: default_exit(),
            edge_threshold_cents: default_edge(),
            safety_margin_cents: default_safety(),
            max_position_cents: default_max_position(),
            max_trades_per_run: default_max_trades(),
            max_spread_cents: default_max_spread(),
            min_hours_before_close: default_min_hours(),
            max_days_to_resolution: default_max_days_to_resolution(),
        }
    }
}

impl Default for TimingConfig {
    fn default() -> Self {
        Self {
            scan_interval_secs: default_scan_interval(),
            forecast_interval_secs: default_forecast_interval(),
            discovery_interval_secs: default_discovery_interval(),
            price_stale_secs: default_price_stale(),
            forecast_stale_secs: default_forecast_stale(),
        }
    }
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            max_position_cents: default_max_position(),
            max_total_exposure_cents: default_max_total_exposure(),
            max_city_exposure_cents: default_max_city_exposure(),
            max_daily_loss_cents: default_max_daily_loss(),
            max_orders_per_minute: default_max_orders_per_min(),
            min_balance_cents: default_min_balance(),
        }
    }
}

impl Default for WeatherSourcesConfig {
    fn default() -> Self {
        Self {
            noaa_weight: default_noaa_weight(),
            google_weight: default_google_weight(),
        }
    }
}

impl Default for QualityConfig {
    fn default() -> Self {
        Self {
            mode: default_quality_mode(),
            strict_source_veto: default_true(),
            require_both_sources: default_true(),
            max_source_prob_gap: default_max_source_prob_gap(),
            min_source_confidence: default_min_source_confidence(),
            min_ensemble_confidence: default_min_ensemble_confidence(),
            min_conservative_net_edge_cents: default_min_conservative_net_edge_cents(),
            min_conservative_ev_cents: default_min_conservative_ev_cents(),
            min_volume_24h: default_min_volume_24h(),
            min_open_interest: default_min_open_interest(),
            slippage_buffer_cents: default_slippage_buffer_cents(),
            max_spread_cents_ultra: default_max_spread_cents_ultra(),
        }
    }
}

impl Default for BotConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            secret_key: String::new(),
            use_demo: true,
            cities: default_cities(),
            series_prefixes: default_series_prefixes(),
            strategy: StrategyConfig::default(),
            risk: RiskConfig::default(),
            timing: TimingConfig::default(),
            weather_sources: WeatherSourcesConfig::default(),
            google_weather_api_key: String::new(),
            quality: QualityConfig::default(),
        }
    }
}
