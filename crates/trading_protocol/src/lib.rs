use exchange_core::{
    AssetClass, BalanceSnapshot, FillReport, NormalizedOrderRequest, OpenOrderSnapshot, OrderAck,
    OrderSnapshot, PositionSnapshot,
};
use serde::{Deserialize, Serialize};
use tokio_util::codec::LengthDelimitedCodec;
use uuid::Uuid;

pub const PROTOCOL_VERSION: u8 = 1;
pub const STATUS_SCHEMA_VERSION: u16 = 3;
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
    #[serde(rename = "Control.Capabilities")]
    Capabilities,
}

impl ControlCommand {
    pub fn as_kind(self) -> &'static str {
        match self {
            Self::Start => "Control.Start",
            Self::Stop => "Control.Stop",
            Self::Status => "Control.Status",
            Self::Ping => "Control.Ping",
            Self::Capabilities => "Control.Capabilities",
        }
    }

    pub fn from_kind(kind: &str) -> Option<Self> {
        match kind {
            "Control.Start" => Some(Self::Start),
            "Control.Stop" => Some(Self::Stop),
            "Control.Status" => Some(Self::Status),
            "Control.Ping" => Some(Self::Ping),
            "Control.Capabilities" => Some(Self::Capabilities),
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineMode {
    #[serde(rename = "paper")]
    Paper,
    #[serde(rename = "hitl_live")]
    HitlLive,
    #[serde(rename = "auto_live")]
    AutoLive,
}

impl EngineMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Paper => "paper",
            Self::HitlLive => "hitl_live",
            Self::AutoLive => "auto_live",
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
    #[serde(rename = "Engine.GetMode")]
    GetMode,
    #[serde(rename = "Engine.SetMode")]
    SetMode,
}

impl EngineCommand {
    pub fn as_kind(self) -> &'static str {
        match self {
            Self::Status => "Engine.Status",
            Self::Pause => "Engine.Pause",
            Self::Resume => "Engine.Resume",
            Self::KillSwitch => "Engine.KillSwitch",
            Self::GetMode => "Engine.GetMode",
            Self::SetMode => "Engine.SetMode",
        }
    }

    pub fn from_kind(kind: &str) -> Option<Self> {
        match kind {
            "Engine.Status" => Some(Self::Status),
            "Engine.Pause" => Some(Self::Pause),
            "Engine.Resume" => Some(Self::Resume),
            "Engine.KillSwitch" => Some(Self::KillSwitch),
            "Engine.GetMode" => Some(Self::GetMode),
            "Engine.SetMode" => Some(Self::SetMode),
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
pub enum ExecutionCommand {
    #[serde(rename = "Execution.Place")]
    Place,
    #[serde(rename = "Execution.Cancel")]
    Cancel,
    #[serde(rename = "Execution.Get")]
    Get,
    #[serde(rename = "Execution.OpenOrders")]
    OpenOrders,
    #[serde(rename = "Execution.Fills")]
    Fills,
}

impl ExecutionCommand {
    pub fn as_kind(self) -> &'static str {
        match self {
            Self::Place => "Execution.Place",
            Self::Cancel => "Execution.Cancel",
            Self::Get => "Execution.Get",
            Self::OpenOrders => "Execution.OpenOrders",
            Self::Fills => "Execution.Fills",
        }
    }

    pub fn from_kind(kind: &str) -> Option<Self> {
        match kind {
            "Execution.Place" => Some(Self::Place),
            "Execution.Cancel" => Some(Self::Cancel),
            "Execution.Get" => Some(Self::Get),
            "Execution.OpenOrders" => Some(Self::OpenOrders),
            "Execution.Fills" => Some(Self::Fills),
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortfolioCommand {
    #[serde(rename = "Portfolio.Positions")]
    Positions,
    #[serde(rename = "Portfolio.Balances")]
    Balances,
    #[serde(rename = "Portfolio.Exposure")]
    Exposure,
}

impl PortfolioCommand {
    pub fn as_kind(self) -> &'static str {
        match self {
            Self::Positions => "Portfolio.Positions",
            Self::Balances => "Portfolio.Balances",
            Self::Exposure => "Portfolio.Exposure",
        }
    }

    pub fn from_kind(kind: &str) -> Option<Self> {
        match kind {
            "Portfolio.Positions" => Some(Self::Positions),
            "Portfolio.Balances" => Some(Self::Balances),
            "Portfolio.Exposure" => Some(Self::Exposure),
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
    Execution(ExecutionCommand),
    Portfolio(PortfolioCommand),
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
        if let Some(cmd) = ExecutionCommand::from_kind(kind) {
            return Some(Self::Execution(cmd));
        }
        PortfolioCommand::from_kind(kind).map(Self::Portfolio)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RoutingCountersPayload {
    pub live_count: u64,
    pub paper_count: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ScopedKillSwitchesPayload {
    pub global: bool,
    pub venues: Vec<String>,
    pub strategies: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ExecutionStatsPayload {
    pub accepted: u64,
    pub rejected: u64,
    pub canceled: u64,
    pub fills: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct VenueExposurePayload {
    pub venue: String,
    pub notional_cents: i64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AssetClassExposurePayload {
    pub asset_class: AssetClass,
    pub notional_cents: i64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PortfolioSummaryPayload {
    pub total_notional_cents: i64,
    pub by_venue: Vec<VenueExposurePayload>,
    pub by_asset_class: Vec<AssetClassExposurePayload>,
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
    pub mode: EngineMode,
    pub routing_counters: RoutingCountersPayload,
    pub scoped_kill_switches: ScopedKillSwitchesPayload,
    pub execution_stats: ExecutionStatsPayload,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DaemonBuildPayload {
    pub name: String,
    pub version: String,
    pub git_sha: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CapabilitiesPayload {
    pub protocol_version: u8,
    pub status_schema_version: u16,
    pub command_kinds_supported: Vec<String>,
    pub daemon_build: DaemonBuildPayload,
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
    pub scoped_kill_switches: ScopedKillSwitchesPayload,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RiskOverridePayload {
    pub action: String,
    pub value: Option<serde_json::Value>,
    pub venue: Option<String>,
    pub strategy_id: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RiskScopedOverridePayload {
    pub action: String,
    pub venue: Option<String>,
    pub strategy_id: Option<String>,
    pub value: Option<serde_json::Value>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct EngineModePayload {
    pub mode: EngineMode,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ExecutionPlacePayload {
    pub order: NormalizedOrderRequest,
    pub approval_token: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ExecutionCancelPayload {
    pub venue_order_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ExecutionGetPayload {
    pub venue_order_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ExecutionFillsPayload {
    pub since_ts_ms: Option<i64>,
    pub limit: Option<usize>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ExecutionPlaceResultPayload {
    pub ack: OrderAck,
    pub order: Option<OrderSnapshot>,
    pub fill: Option<FillReport>,
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
        simulated: bool,
        venue_order_id: Option<String>,
    },
    #[serde(rename = "Event.AgentCodegen")]
    AgentCodegen {
        strategy_id: String,
        phase: String,
        code_hash: String,
        passed: bool,
        reason: Option<String>,
    },
    #[serde(rename = "Event.PortfolioSync")]
    PortfolioSync { positions: usize, balances: usize },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PortfolioPositionsPayload {
    pub positions: Vec<PositionSnapshot>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PortfolioBalancesPayload {
    pub balances: Vec<BalanceSnapshot>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ExecutionOpenOrdersPayload {
    pub orders: Vec<OpenOrderSnapshot>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ExecutionFillsResultPayload {
    pub fills: Vec<FillReport>,
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
            RequestKind::from_kind("Control.Capabilities"),
            Some(RequestKind::Control(ControlCommand::Capabilities))
        );
        assert_eq!(
            RequestKind::from_kind("Engine.Pause"),
            Some(RequestKind::Engine(EngineCommand::Pause))
        );
        assert_eq!(
            RequestKind::from_kind("Engine.SetMode"),
            Some(RequestKind::Engine(EngineCommand::SetMode))
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
            RequestKind::from_kind("Execution.Place"),
            Some(RequestKind::Execution(ExecutionCommand::Place))
        );
        assert_eq!(
            RequestKind::from_kind("Portfolio.Exposure"),
            Some(RequestKind::Portfolio(PortfolioCommand::Exposure))
        );
        assert_eq!(RequestKind::from_kind("Unknown.Command"), None);
    }

    #[test]
    fn response_keeps_correlation_id() {
        let req = Envelope::new("Engine.Status", json!({}));
        let resp = Envelope::response_to(&req, json!({ "ok": true }));

        assert_eq!(resp.id, req.id);
        assert_eq!(resp.kind, "Engine.Status.Response");
    }
}
