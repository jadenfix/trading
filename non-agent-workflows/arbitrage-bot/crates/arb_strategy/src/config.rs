//! Configuration structs for arb strategy.

use serde::{Deserialize, Serialize};

/// Arb strategy thresholds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArbConfig {
    #[serde(default = "default_min_profit")]
    pub min_profit_cents: i64,

    #[serde(default = "default_slippage")]
    pub slippage_buffer_cents: i64,

    #[serde(default = "default_true")]
    pub ev_mode_enabled: bool,

    #[serde(default = "default_true")]
    pub guaranteed_arb_only: bool,

    #[serde(default = "default_true")]
    pub strict_binary_only: bool,

    #[serde(default = "default_tie_buffer")]
    pub tie_buffer_cents: i64,

    #[serde(default = "default_min_leg_size")]
    pub min_leg_size: i64,

    #[serde(default = "default_qty")]
    pub default_qty: i64,
}

/// Timing configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArbTimingConfig {
    #[serde(default = "default_universe_refresh")]
    pub universe_refresh_secs: u64,

    #[serde(default = "default_eval_interval")]
    pub eval_interval_ms: u64,

    #[serde(default = "default_quote_stale")]
    pub quote_stale_secs: u64,

    #[serde(default = "default_price_stale")]
    pub price_stale_secs: u64,
}

/// Risk management thresholds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArbRiskConfig {
    #[serde(default = "default_max_event_exposure")]
    pub max_exposure_per_event_cents: i64,

    #[serde(default = "default_max_total_exposure")]
    pub max_total_exposure_cents: i64,

    #[serde(default = "default_max_attempts")]
    pub max_attempts_per_group_per_min: u32,

    #[serde(default = "default_max_unwind_loss")]
    pub max_unwind_loss_cents: i64,

    #[serde(default = "default_kill_switch")]
    pub kill_switch_disconnect_count: u32,

    #[serde(default = "default_max_orders_per_min")]
    pub max_orders_per_minute: u32,

    #[serde(default = "default_min_balance")]
    pub min_balance_cents: i64,

    #[serde(default = "default_max_daily_loss")]
    pub max_daily_loss_cents: i64,
}

/// Execution configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionConfig {
    #[serde(default = "default_tif")]
    pub time_in_force: String,

    #[serde(default = "default_unwind_policy")]
    pub unwind_policy: String,
}

// ── Defaults ──────────────────────────────────────────────────────────

fn default_true() -> bool {
    true
}
fn default_min_profit() -> i64 {
    1
}
fn default_slippage() -> i64 {
    1
}
fn default_tie_buffer() -> i64 {
    5
}
fn default_min_leg_size() -> i64 {
    5
}
fn default_qty() -> i64 {
    10
}
fn default_universe_refresh() -> u64 {
    900
}
fn default_eval_interval() -> u64 {
    1000
}
fn default_quote_stale() -> u64 {
    120
}
fn default_price_stale() -> u64 {
    300
}
fn default_max_event_exposure() -> i64 {
    1000
}
fn default_max_total_exposure() -> i64 {
    5000
}
fn default_max_attempts() -> u32 {
    3
}
fn default_max_unwind_loss() -> i64 {
    50
}
fn default_kill_switch() -> u32 {
    5
}
fn default_max_orders_per_min() -> u32 {
    10
}
fn default_min_balance() -> i64 {
    100
}
fn default_max_daily_loss() -> i64 {
    2000
}
fn default_tif() -> String {
    "fok".into()
}
fn default_unwind_policy() -> String {
    "cross_spread".into()
}

impl Default for ArbConfig {
    fn default() -> Self {
        Self {
            min_profit_cents: default_min_profit(),
            slippage_buffer_cents: default_slippage(),
            ev_mode_enabled: true,
            guaranteed_arb_only: true,
            strict_binary_only: true,
            tie_buffer_cents: default_tie_buffer(),
            min_leg_size: default_min_leg_size(),
            default_qty: default_qty(),
        }
    }
}

impl Default for ArbTimingConfig {
    fn default() -> Self {
        Self {
            universe_refresh_secs: default_universe_refresh(),
            eval_interval_ms: default_eval_interval(),
            quote_stale_secs: default_quote_stale(),
            price_stale_secs: default_price_stale(),
        }
    }
}

impl Default for ArbRiskConfig {
    fn default() -> Self {
        Self {
            max_exposure_per_event_cents: default_max_event_exposure(),
            max_total_exposure_cents: default_max_total_exposure(),
            max_attempts_per_group_per_min: default_max_attempts(),
            max_unwind_loss_cents: default_max_unwind_loss(),
            kill_switch_disconnect_count: default_kill_switch(),
            max_orders_per_minute: default_max_orders_per_min(),
            min_balance_cents: default_min_balance(),
            max_daily_loss_cents: default_max_daily_loss(),
        }
    }
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            time_in_force: default_tif(),
            unwind_policy: default_unwind_policy(),
        }
    }
}
