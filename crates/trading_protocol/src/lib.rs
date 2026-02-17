use serde::{Deserialize, Serialize};
use tokio_util::codec::LengthDelimitedCodec;
use uuid::Uuid;

pub const PROTOCOL_VERSION: u8 = 1;
pub const DEFAULT_SOCKET_PATH: &str = "/var/run/openclaw/trading.sock";
pub const MAX_FRAME_LENGTH: usize = 1024 * 1024; // 1 MiB

/// 4-byte Big-Endian length-prefixed JSON codec.
pub type JsonCodec = LengthDelimitedCodec;

pub fn create_codec() -> JsonCodec {
    LengthDelimitedCodec::builder()
        .length_field_length(4)
        .max_frame_length(MAX_FRAME_LENGTH)
        .new_codec()
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Envelope {
    pub v: u8,
    pub id: Uuid,
    #[serde(rename = "type")]
    pub kind: String,
    pub ts_ms: i64,
    pub payload: serde_json::Value,
}

impl Envelope {
    pub fn new(kind: &str, payload: serde_json::Value) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id: Uuid::new_v4(),
            kind: kind.to_string(),
            ts_ms: chrono::Utc::now().timestamp_millis(),
            payload,
        }
    }

    pub fn response_to(req: &Envelope, payload: serde_json::Value) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id: req.id,
            kind: format!("{}.Response", req.kind),
            ts_ms: chrono::Utc::now().timestamp_millis(),
            payload,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlCommand {
    #[serde(rename = "Control.Start")]
    Start,
    #[serde(rename = "Control.Stop")]
    Stop,
    #[serde(rename = "Control.Status")]
    Status,
    #[serde(rename = "Control.Ping")]
    Ping,
}

impl ControlCommand {
    pub fn as_kind(self) -> &'static str {
        match self {
            Self::Start => "Control.Start",
            Self::Stop => "Control.Stop",
            Self::Status => "Control.Status",
            Self::Ping => "Control.Ping",
        }
    }

    pub fn from_kind(kind: &str) -> Option<Self> {
        match kind {
            "Control.Start" => Some(Self::Start),
            "Control.Stop" => Some(Self::Stop),
            "Control.Status" => Some(Self::Status),
            "Control.Ping" => Some(Self::Ping),
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineCommand {
    #[serde(rename = "Engine.Status")]
    Status,
    #[serde(rename = "Engine.Pause")]
    Pause,
    #[serde(rename = "Engine.Resume")]
    Resume,
    #[serde(rename = "Engine.KillSwitch")]
    KillSwitch,
}

impl EngineCommand {
    pub fn as_kind(self) -> &'static str {
        match self {
            Self::Status => "Engine.Status",
            Self::Pause => "Engine.Pause",
            Self::Resume => "Engine.Resume",
            Self::KillSwitch => "Engine.KillSwitch",
        }
    }

    pub fn from_kind(kind: &str) -> Option<Self> {
        match kind {
            "Engine.Status" => Some(Self::Status),
            "Engine.Pause" => Some(Self::Pause),
            "Engine.Resume" => Some(Self::Resume),
            "Engine.KillSwitch" => Some(Self::KillSwitch),
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrategyCommand {
    #[serde(rename = "Strategy.List")]
    List,
    #[serde(rename = "Strategy.Enable")]
    Enable,
    #[serde(rename = "Strategy.Disable")]
    Disable,
    #[serde(rename = "Strategy.UploadCandidate")]
    UploadCandidate,
    #[serde(rename = "Strategy.PromoteCandidate")]
    PromoteCandidate,
}

impl StrategyCommand {
    pub fn as_kind(self) -> &'static str {
        match self {
            Self::List => "Strategy.List",
            Self::Enable => "Strategy.Enable",
            Self::Disable => "Strategy.Disable",
            Self::UploadCandidate => "Strategy.UploadCandidate",
            Self::PromoteCandidate => "Strategy.PromoteCandidate",
        }
    }

    pub fn from_kind(kind: &str) -> Option<Self> {
        match kind {
            "Strategy.List" => Some(Self::List),
            "Strategy.Enable" => Some(Self::Enable),
            "Strategy.Disable" => Some(Self::Disable),
            "Strategy.UploadCandidate" => Some(Self::UploadCandidate),
            "Strategy.PromoteCandidate" => Some(Self::PromoteCandidate),
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskCommand {
    #[serde(rename = "Risk.Status")]
    Status,
    #[serde(rename = "Risk.Override")]
    Override,
}

impl RiskCommand {
    pub fn as_kind(self) -> &'static str {
        match self {
            Self::Status => "Risk.Status",
            Self::Override => "Risk.Override",
        }
    }

    pub fn from_kind(kind: &str) -> Option<Self> {
        match kind {
            "Risk.Status" => Some(Self::Status),
            "Risk.Override" => Some(Self::Override),
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum VenueCommand {
    #[serde(rename = "Venue.List")]
    List,
    #[serde(rename = "Venue.Enable")]
    Enable,
    #[serde(rename = "Venue.Disable")]
    Disable,
    #[serde(rename = "Venue.Status")]
    Status,
}

impl VenueCommand {
    pub fn as_kind(self) -> &'static str {
        match self {
            Self::List => "Venue.List",
            Self::Enable => "Venue.Enable",
            Self::Disable => "Venue.Disable",
            Self::Status => "Venue.Status",
        }
    }

    pub fn from_kind(kind: &str) -> Option<Self> {
        match kind {
            "Venue.List" => Some(Self::List),
            "Venue.Enable" => Some(Self::Enable),
            "Venue.Disable" => Some(Self::Disable),
            "Venue.Status" => Some(Self::Status),
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortfolioCommand {
    #[serde(rename = "Portfolio.Balances")]
    Balances,
    #[serde(rename = "Portfolio.Positions")]
    Positions,
}

impl PortfolioCommand {
    pub fn as_kind(self) -> &'static str {
        match self {
            Self::Balances => "Portfolio.Balances",
            Self::Positions => "Portfolio.Positions",
        }
    }

    pub fn from_kind(kind: &str) -> Option<Self> {
        match kind {
            "Portfolio.Balances" => Some(Self::Balances),
            "Portfolio.Positions" => Some(Self::Positions),
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderCommand {
    #[serde(rename = "Order.Submit")]
    Submit,
    #[serde(rename = "Order.Cancel")]
    Cancel,
    #[serde(rename = "Order.List")]
    List,
}

impl OrderCommand {
    pub fn as_kind(self) -> &'static str {
        match self {
            Self::Submit => "Order.Submit",
            Self::Cancel => "Order.Cancel",
            Self::List => "Order.List",
        }
    }

    pub fn from_kind(kind: &str) -> Option<Self> {
        match kind {
            "Order.Submit" => Some(Self::Submit),
            "Order.Cancel" => Some(Self::Cancel),
            "Order.List" => Some(Self::List),
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionModeCommand {
    #[serde(rename = "ExecutionMode.Set")]
    Set,
    #[serde(rename = "ExecutionMode.Get")]
    Get,
}

impl ExecutionModeCommand {
    pub fn as_kind(self) -> &'static str {
        match self {
            Self::Set => "ExecutionMode.Set",
            Self::Get => "ExecutionMode.Get",
        }
    }

    pub fn from_kind(kind: &str) -> Option<Self> {
        match kind {
            "ExecutionMode.Set" => Some(Self::Set),
            "ExecutionMode.Get" => Some(Self::Get),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestKind {
    Control(ControlCommand),
    Engine(EngineCommand),
    Strategy(StrategyCommand),
    Risk(RiskCommand),
    Venue(VenueCommand),
    Portfolio(PortfolioCommand),
    Order(OrderCommand),
    ExecutionMode(ExecutionModeCommand),
}

impl RequestKind {
    pub fn from_kind(kind: &str) -> Option<Self> {
        if let Some(cmd) = ControlCommand::from_kind(kind) {
            return Some(Self::Control(cmd));
        }
        if let Some(cmd) = EngineCommand::from_kind(kind) {
            return Some(Self::Engine(cmd));
        }
        if let Some(cmd) = StrategyCommand::from_kind(kind) {
            return Some(Self::Strategy(cmd));
        }
        if let Some(cmd) = RiskCommand::from_kind(kind) {
            return Some(Self::Risk(cmd));
        }
        if let Some(cmd) = VenueCommand::from_kind(kind) {
            return Some(Self::Venue(cmd));
        }
        if let Some(cmd) = PortfolioCommand::from_kind(kind) {
            return Some(Self::Portfolio(cmd));
        }
        if let Some(cmd) = OrderCommand::from_kind(kind) {
            return Some(Self::Order(cmd));
        }
        ExecutionModeCommand::from_kind(kind).map(Self::ExecutionMode)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct EngineStatePayload {
    pub running: bool,
    pub paused: bool,
    pub kill_switch_engaged: bool,
    pub risk_tripped: bool,
    pub started_at_ms: i64,
    pub last_command_at_ms: i64,
    pub strategies_total: usize,
    pub strategies_enabled: usize,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StrategySummaryPayload {
    pub id: String,
    pub enabled: bool,
    pub family: String,
    pub source: String,
    pub version: u64,
    pub canary_deployment: bool,
    pub canary_notional_cents: i64,
    pub active_code_hash: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CandidateUploadPayload {
    pub strategy_id: String,
    pub source: String,
    pub code_hash: String,
    pub requested_canary_notional_cents: i64,
    pub compile_passed: bool,
    pub replay_passed: bool,
    pub paper_passed: bool,
    pub latency_passed: bool,
    pub risk_passed: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CandidatePromotePayload {
    pub strategy_id: String,
    pub code_hash: String,
    pub requested_canary_notional_cents: i64,
    pub auto: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RiskLimitsPayload {
    pub max_total_notional_cents: i64,
    pub max_strategy_canary_notional_cents: i64,
    pub max_orders_per_minute: u32,
    pub max_drawdown_cents: i64,
    pub forced_cooldown_secs: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RiskStatePayload {
    pub kill_switch_engaged: bool,
    pub paused: bool,
    pub orders_last_minute: u32,
    pub drawdown_cents: i64,
    pub total_notional_cents: i64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RiskOverridePayload {
    pub action: String,
    pub value: Option<serde_json::Value>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct VenueTogglePayload {
    pub venue_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct VenueSummaryPayload {
    pub venue_id: String,
    pub enabled: bool,
    pub market_types: Vec<String>,
    pub healthy: bool,
    pub live_enabled: bool,
    pub paper_only: bool,
    pub message: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct InstrumentRefPayload {
    pub venue_id: String,
    pub symbol: String,
    pub asset_class: String,
    pub market_type: String,
    pub expiry_ts_ms: Option<i64>,
    pub strike: Option<f64>,
    pub option_type: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PortfolioBalancePayload {
    pub venue_id: String,
    pub asset: String,
    pub total: f64,
    pub available: f64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PortfolioPositionPayload {
    pub venue_id: String,
    pub instrument: InstrumentRefPayload,
    pub quantity: f64,
    pub avg_price: f64,
    pub mark_price: Option<f64>,
    pub unrealized_pnl: Option<f64>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct OrderSubmitPayload {
    pub strategy_id: String,
    pub venue_id: String,
    pub instrument: InstrumentRefPayload,
    pub side: String,
    pub order_type: String,
    pub quantity: f64,
    pub limit_price: Option<f64>,
    pub tif: Option<String>,
    pub post_only: bool,
    pub reduce_only: bool,
    pub client_order_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct OrderCancelPayload {
    pub venue_id: Option<String>,
    pub venue_order_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct OrderListItemPayload {
    pub venue_order_id: String,
    pub client_order_id: String,
    pub strategy_id: String,
    pub venue_id: String,
    pub instrument: InstrumentRefPayload,
    pub side: String,
    pub order_type: String,
    pub quantity: f64,
    pub filled_quantity: f64,
    pub avg_fill_price: Option<f64>,
    pub status: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub message: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ExecutionModePayload {
    pub venue_id: String,
    pub market_type: String,
    pub mode: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum Event {
    #[serde(rename = "Event.Alert")]
    Alert { level: String, message: String },
    #[serde(rename = "Event.EngineState")]
    EngineState { running: bool, risk_tripped: bool },
    #[serde(rename = "Event.EngineHealth")]
    EngineHealth {
        running: bool,
        paused: bool,
        kill_switch_engaged: bool,
        risk_tripped: bool,
    },
    #[serde(rename = "Event.RiskAlert")]
    RiskAlert {
        level: String,
        reason: String,
        kill_switch_engaged: bool,
    },
    #[serde(rename = "Event.StrategyLifecycle")]
    StrategyLifecycle {
        strategy_id: String,
        phase: String,
        version: u64,
        code_hash: Option<String>,
    },
    #[serde(rename = "Event.Execution")]
    Execution {
        venue: String,
        strategy_id: String,
        symbol: String,
        action: String,
        status: String,
        latency_ms: u64,
    },
    #[serde(rename = "Event.AgentCodegen")]
    AgentCodegen {
        strategy_id: String,
        phase: String,
        code_hash: String,
        passed: bool,
        reason: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn request_kind_parses_all_commands() {
        assert_eq!(
            RequestKind::from_kind("Control.Status"),
            Some(RequestKind::Control(ControlCommand::Status))
        );
        assert_eq!(
            RequestKind::from_kind("Engine.Pause"),
            Some(RequestKind::Engine(EngineCommand::Pause))
        );
        assert_eq!(
            RequestKind::from_kind("Strategy.PromoteCandidate"),
            Some(RequestKind::Strategy(StrategyCommand::PromoteCandidate))
        );
        assert_eq!(
            RequestKind::from_kind("Risk.Override"),
            Some(RequestKind::Risk(RiskCommand::Override))
        );
        assert_eq!(
            RequestKind::from_kind("Venue.List"),
            Some(RequestKind::Venue(VenueCommand::List))
        );
        assert_eq!(
            RequestKind::from_kind("Portfolio.Balances"),
            Some(RequestKind::Portfolio(PortfolioCommand::Balances))
        );
        assert_eq!(
            RequestKind::from_kind("Order.Submit"),
            Some(RequestKind::Order(OrderCommand::Submit))
        );
        assert_eq!(
            RequestKind::from_kind("ExecutionMode.Get"),
            Some(RequestKind::ExecutionMode(ExecutionModeCommand::Get))
        );
        assert_eq!(RequestKind::from_kind("Unknown.Command"), None);
    }

    #[test]
    fn response_keeps_correlation_id() {
        let req = Envelope::new("Engine.Status", json!({}));
        let resp = Envelope::response_to(&req, json!({"ok": true}));

        assert_eq!(resp.id, req.id);
        assert_eq!(resp.kind, "Engine.Status.Response");
    }
}
