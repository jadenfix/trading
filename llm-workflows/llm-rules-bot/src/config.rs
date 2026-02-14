use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub kalshi: KalshiConfig,
    pub llm: LlmConfig,
    pub llm_budget: LlmBudgetConfig,
    #[serde(default)]
    pub temporal: TemporalConfig,
    pub research: ResearchConfig,
    pub trading: TradingConfig,
    pub risk: RiskConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct KalshiConfig {
    pub use_demo: bool,
    pub api_base: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LlmConfig {
    pub provider: String,
    pub model: String,
    pub timeout_ms: u64,
    pub max_retries: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LlmBudgetConfig {
    pub daily_max_calls: u32,
    pub per_market_max_calls: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResearchConfig {
    pub min_edge_for_research_cents: i64,
    pub complexity_threshold: f64,
    pub market_liquidity_min_depth: i64,
    pub max_days_to_expiry: i64,
    pub max_uncertainty: f64,
    pub rules_risk_veto_level: String,
    pub staleness_budget_ms: i64,
    pub llm_required_for_complex_markets: bool,
    #[serde(default)]
    pub allowed_urls: Vec<String>,
    #[serde(default = "default_kelly_fraction")]
    pub kelly_fraction: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TradingConfig {
    pub shadow_mode: bool,
    pub live_enable: bool,
    pub live_min_size_contracts: i64,
    pub max_position_contracts: i64,
    pub loop_interval_ms: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RiskConfig {
    pub max_position_cents: i64,
    pub max_total_exposure_cents: i64,
    pub max_daily_loss_cents: i64,
    pub max_orders_per_minute: u32,
    pub min_balance_cents: i64,
}

fn default_kelly_fraction() -> f64 {
    0.2
}

#[derive(Debug, Clone, Deserialize)]
pub struct TemporalConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_broker_base_url")]
    pub broker_base_url: String,
    #[serde(default = "default_temporal_start_path")]
    pub start_path: String,
    #[serde(default = "default_temporal_poll_path_template")]
    pub poll_path_template: String,
    #[serde(default = "default_temporal_poll_interval")]
    pub poll_interval_ms: u64,
    #[serde(default = "default_temporal_workflow_timeout")]
    pub workflow_timeout_ms: u64,
    #[serde(default = "default_temporal_request_timeout")]
    pub request_timeout_ms: u64,
    #[serde(default)]
    pub auth_token_env: String,
}

impl Default for TemporalConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            broker_base_url: default_broker_base_url(),
            start_path: default_temporal_start_path(),
            poll_path_template: default_temporal_poll_path_template(),
            poll_interval_ms: default_temporal_poll_interval(),
            workflow_timeout_ms: default_temporal_workflow_timeout(),
            request_timeout_ms: default_temporal_request_timeout(),
            auth_token_env: String::new(),
        }
    }
}

fn default_broker_base_url() -> String {
    "http://127.0.0.1:8787".into()
}

fn default_temporal_start_path() -> String {
    "/research/start".into()
}

fn default_temporal_poll_path_template() -> String {
    "/research/{id}".into()
}

fn default_temporal_poll_interval() -> u64 {
    250
}

fn default_temporal_workflow_timeout() -> u64 {
    5_000
}

fn default_temporal_request_timeout() -> u64 {
    2_000
}

impl AppConfig {
    pub fn load(path: &str) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: AppConfig = toml::from_str(&content)?;
        Ok(config)
    }
}
