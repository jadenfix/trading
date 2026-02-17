use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use exchange_core::{
    AssetClass, InstrumentRef, InstrumentType, NormalizedOrderRequest, OptionRight, OrderSide,
    OrderType, TimeInForce,
};
use futures::{SinkExt, StreamExt};
use tokio::net::UnixStream;
use tokio_util::codec::Framed;
use trading_protocol::{
    create_codec, CandidatePromotePayload, CandidateUploadPayload, ControlCommand, EngineCommand,
    EngineMode, EngineModePayload, Envelope, ExecutionCancelPayload, ExecutionCommand,
    ExecutionFillsPayload, ExecutionGetPayload, ExecutionPlacePayload, PortfolioCommand,
    RiskCommand, RiskOverridePayload, RiskScopedOverridePayload, StrategyCommand,
    DEFAULT_SOCKET_PATH,
};
use uuid::Uuid;

#[derive(Parser)]
#[command(name = "tradingctl")]
#[command(about = "CLI for OpenClaw Trading Daemon", long_about = None)]
struct Cli {
    /// UDS socket path override.
    #[arg(long, default_value = DEFAULT_SOCKET_PATH)]
    socket: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ModeArg {
    Paper,
    HitlLive,
    AutoLive,
}

impl From<ModeArg> for EngineMode {
    fn from(value: ModeArg) -> Self {
        match value {
            ModeArg::Paper => EngineMode::Paper,
            ModeArg::HitlLive => EngineMode::HitlLive,
            ModeArg::AutoLive => EngineMode::AutoLive,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum AssetClassArg {
    Crypto,
    Equity,
    Prediction,
    Fx,
    Rates,
    Commodity,
    Custom,
}

impl From<AssetClassArg> for AssetClass {
    fn from(value: AssetClassArg) -> Self {
        match value {
            AssetClassArg::Crypto => AssetClass::Crypto,
            AssetClassArg::Equity => AssetClass::Equity,
            AssetClassArg::Prediction => AssetClass::Prediction,
            AssetClassArg::Fx => AssetClass::Fx,
            AssetClassArg::Rates => AssetClass::Rates,
            AssetClassArg::Commodity => AssetClass::Commodity,
            AssetClassArg::Custom => AssetClass::Custom,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum InstrumentTypeArg {
    Spot,
    Perpetual,
    Future,
    Option,
    BinaryOption,
    Custom,
}

impl From<InstrumentTypeArg> for InstrumentType {
    fn from(value: InstrumentTypeArg) -> Self {
        match value {
            InstrumentTypeArg::Spot => InstrumentType::Spot,
            InstrumentTypeArg::Perpetual => InstrumentType::Perpetual,
            InstrumentTypeArg::Future => InstrumentType::Future,
            InstrumentTypeArg::Option => InstrumentType::Option,
            InstrumentTypeArg::BinaryOption => InstrumentType::BinaryOption,
            InstrumentTypeArg::Custom => InstrumentType::Custom,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum SideArg {
    Buy,
    Sell,
}

impl From<SideArg> for OrderSide {
    fn from(value: SideArg) -> Self {
        match value {
            SideArg::Buy => OrderSide::Buy,
            SideArg::Sell => OrderSide::Sell,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum OrderTypeArg {
    Limit,
    Market,
}

impl From<OrderTypeArg> for OrderType {
    fn from(value: OrderTypeArg) -> Self {
        match value {
            OrderTypeArg::Limit => OrderType::Limit,
            OrderTypeArg::Market => OrderType::Market,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum TifArg {
    Gtc,
    Ioc,
    Fok,
    Day,
}

impl From<TifArg> for TimeInForce {
    fn from(value: TifArg) -> Self {
        match value {
            TifArg::Gtc => TimeInForce::Gtc,
            TifArg::Ioc => TimeInForce::Ioc,
            TifArg::Fok => TimeInForce::Fok,
            TifArg::Day => TimeInForce::Day,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum OptionRightArg {
    Call,
    Put,
}

impl From<OptionRightArg> for OptionRight {
    fn from(value: OptionRightArg) -> Self {
        match value {
            OptionRightArg::Call => OptionRight::Call,
            OptionRightArg::Put => OptionRight::Put,
        }
    }
}

#[derive(Subcommand)]
enum Commands {
    /// Legacy status command (maps to Engine.Status)
    Status,
    /// Send Control.Start command
    Start,
    /// Send Control.Stop command
    Stop,
    /// Send Control.Ping command
    Ping,
    /// Send Control.Capabilities command
    Capabilities,
    /// Send Engine.Status command
    EngineStatus,
    /// Send Engine.Pause command
    Pause,
    /// Send Engine.Resume command
    Resume,
    /// Send Engine.KillSwitch command
    KillSwitch,
    /// Send Engine.GetMode command
    EngineGetMode,
    /// Send Engine.SetMode command
    EngineSetMode {
        #[arg(long, value_enum)]
        mode: ModeArg,
    },
    /// Send Strategy.List command
    StrategyList,
    /// Send Strategy.Enable command
    StrategyEnable {
        #[arg(long)]
        strategy_id: String,
    },
    /// Send Strategy.Disable command
    StrategyDisable {
        #[arg(long)]
        strategy_id: String,
    },
    /// Upload a strategy candidate package for promotion checks
    StrategyUploadCandidate {
        #[arg(long)]
        strategy_id: String,
        #[arg(long)]
        code_hash: String,
        #[arg(long, default_value = "agent")]
        source: String,
        #[arg(long, default_value_t = 250)]
        requested_canary_notional_cents: i64,
        #[arg(long, default_value_t = true)]
        compile_passed: bool,
        #[arg(long, default_value_t = true)]
        replay_passed: bool,
        #[arg(long, default_value_t = true)]
        paper_passed: bool,
        #[arg(long, default_value_t = true)]
        latency_passed: bool,
        #[arg(long, default_value_t = true)]
        risk_passed: bool,
    },
    /// Promote an uploaded strategy candidate
    StrategyPromoteCandidate {
        #[arg(long)]
        strategy_id: String,
        #[arg(long)]
        code_hash: String,
        #[arg(long, default_value_t = 250)]
        requested_canary_notional_cents: i64,
        #[arg(long, default_value_t = true)]
        auto: bool,
    },
    /// Send Risk.Status command
    RiskStatus,
    /// Send Risk.Override command
    RiskOverride {
        #[arg(long)]
        action: String,
        /// JSON value or raw string
        #[arg(long)]
        value: Option<String>,
        #[arg(long)]
        venue: Option<String>,
        #[arg(long)]
        strategy_id: Option<String>,
    },
    /// Send Risk.Override with scoped payload convenience
    RiskScoped {
        #[arg(long)]
        action: String,
        #[arg(long)]
        venue: Option<String>,
        #[arg(long)]
        strategy_id: Option<String>,
        #[arg(long)]
        value: Option<String>,
    },
    /// Send Execution.Place command
    ExecutionPlace {
        #[arg(long, default_value = "coinbase_at")]
        venue: String,
        #[arg(long)]
        symbol: String,
        #[arg(long)]
        strategy_id: String,
        #[arg(long)]
        client_order_id: Option<String>,
        #[arg(long)]
        intent_id: Option<String>,
        #[arg(long, value_enum)]
        side: SideArg,
        #[arg(long, value_enum)]
        order_type: OrderTypeArg,
        #[arg(long)]
        qty: f64,
        #[arg(long)]
        limit_price: Option<f64>,
        #[arg(long, value_enum)]
        tif: Option<TifArg>,
        #[arg(long, default_value_t = false)]
        post_only: bool,
        #[arg(long, default_value_t = false)]
        reduce_only: bool,
        #[arg(long, value_enum, default_value_t = AssetClassArg::Crypto)]
        asset_class: AssetClassArg,
        #[arg(long, value_enum, default_value_t = InstrumentTypeArg::Spot)]
        instrument_type: InstrumentTypeArg,
        #[arg(long)]
        base: Option<String>,
        #[arg(long)]
        quote: Option<String>,
        #[arg(long)]
        expiry_ts_ms: Option<i64>,
        #[arg(long)]
        strike: Option<f64>,
        #[arg(long, value_enum)]
        option_right: Option<OptionRightArg>,
        #[arg(long)]
        contract_multiplier: Option<f64>,
        #[arg(long, default_value_t = 0)]
        requested_notional_cents: i64,
        #[arg(long)]
        approval_token: Option<String>,
    },
    /// Send Execution.Cancel command
    ExecutionCancel {
        #[arg(long)]
        venue_order_id: String,
    },
    /// Send Execution.Get command
    ExecutionGet {
        #[arg(long)]
        venue_order_id: String,
    },
    /// Send Execution.OpenOrders command
    ExecutionOpenOrders,
    /// Send Execution.Fills command
    ExecutionFills {
        #[arg(long)]
        since_ts_ms: Option<i64>,
        #[arg(long, default_value_t = 200)]
        limit: usize,
    },
    /// Send Portfolio.Positions command
    PortfolioPositions,
    /// Send Portfolio.Balances command
    PortfolioBalances,
    /// Send Portfolio.Exposure command
    PortfolioExposure,
    /// Send a raw JSON command
    Raw {
        #[arg(short, long)]
        json: String,
    },
}

fn parse_risk_override_value(raw: &str) -> serde_json::Value {
    match serde_json::from_str(raw) {
        Ok(value) => value,
        Err(err) => {
            eprintln!(
                "Warning: failed to parse risk override value as JSON ({}). Treating as plain string: {}",
                err, raw
            );
            serde_json::Value::String(raw.to_string())
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let stream = UnixStream::connect(&cli.socket)
        .await
        .context("Failed to connect to daemon socket. Is the daemon running?")?;

    let mut framed = Framed::new(stream, create_codec());

    let (kind, payload) = match cli.command {
        Commands::Status | Commands::EngineStatus => {
            (EngineCommand::Status.as_kind(), serde_json::json!({}))
        }
        Commands::Start => (ControlCommand::Start.as_kind(), serde_json::json!({})),
        Commands::Stop => (ControlCommand::Stop.as_kind(), serde_json::json!({})),
        Commands::Ping => (ControlCommand::Ping.as_kind(), serde_json::json!({})),
        Commands::Capabilities => (
            ControlCommand::Capabilities.as_kind(),
            serde_json::json!({}),
        ),
        Commands::Pause => (EngineCommand::Pause.as_kind(), serde_json::json!({})),
        Commands::Resume => (EngineCommand::Resume.as_kind(), serde_json::json!({})),
        Commands::KillSwitch => (EngineCommand::KillSwitch.as_kind(), serde_json::json!({})),
        Commands::EngineGetMode => (EngineCommand::GetMode.as_kind(), serde_json::json!({})),
        Commands::EngineSetMode { mode } => {
            let payload = EngineModePayload { mode: mode.into() };
            (
                EngineCommand::SetMode.as_kind(),
                serde_json::to_value(payload)?,
            )
        }
        Commands::StrategyList => (StrategyCommand::List.as_kind(), serde_json::json!({})),
        Commands::StrategyEnable { strategy_id } => (
            StrategyCommand::Enable.as_kind(),
            serde_json::json!({ "strategy_id": strategy_id }),
        ),
        Commands::StrategyDisable { strategy_id } => (
            StrategyCommand::Disable.as_kind(),
            serde_json::json!({ "strategy_id": strategy_id }),
        ),
        Commands::StrategyUploadCandidate {
            strategy_id,
            code_hash,
            source,
            requested_canary_notional_cents,
            compile_passed,
            replay_passed,
            paper_passed,
            latency_passed,
            risk_passed,
        } => {
            let payload = CandidateUploadPayload {
                strategy_id,
                source,
                code_hash,
                requested_canary_notional_cents,
                compile_passed,
                replay_passed,
                paper_passed,
                latency_passed,
                risk_passed,
            };
            (
                StrategyCommand::UploadCandidate.as_kind(),
                serde_json::to_value(payload)?,
            )
        }
        Commands::StrategyPromoteCandidate {
            strategy_id,
            code_hash,
            requested_canary_notional_cents,
            auto,
        } => {
            let payload = CandidatePromotePayload {
                strategy_id,
                code_hash,
                requested_canary_notional_cents,
                auto,
            };
            (
                StrategyCommand::PromoteCandidate.as_kind(),
                serde_json::to_value(payload)?,
            )
        }
        Commands::RiskStatus => (RiskCommand::Status.as_kind(), serde_json::json!({})),
        Commands::RiskOverride {
            action,
            value,
            venue,
            strategy_id,
        } => {
            let parsed_value = value.as_ref().map(|raw| parse_risk_override_value(raw));

            let payload = RiskOverridePayload {
                action,
                value: parsed_value,
                venue,
                strategy_id,
            };
            (
                RiskCommand::Override.as_kind(),
                serde_json::to_value(payload)?,
            )
        }
        Commands::RiskScoped {
            action,
            venue,
            strategy_id,
            value,
        } => {
            let parsed_value = value.as_ref().map(|raw| parse_risk_override_value(raw));
            let payload = RiskScopedOverridePayload {
                action,
                venue,
                strategy_id,
                value: parsed_value,
            };
            (
                RiskCommand::Override.as_kind(),
                serde_json::to_value(payload)?,
            )
        }
        Commands::ExecutionPlace {
            venue,
            symbol,
            strategy_id,
            client_order_id,
            intent_id,
            side,
            order_type,
            qty,
            limit_price,
            tif,
            post_only,
            reduce_only,
            asset_class,
            instrument_type,
            base,
            quote,
            expiry_ts_ms,
            strike,
            option_right,
            contract_multiplier,
            requested_notional_cents,
            approval_token,
        } => {
            let client_order_id =
                client_order_id.unwrap_or_else(|| format!("ctl-{}", Uuid::new_v4().as_simple()));
            let instrument = InstrumentRef {
                venue: venue.clone(),
                venue_symbol: symbol.clone(),
                asset_class: asset_class.into(),
                instrument_type: instrument_type.into(),
                base,
                quote,
                expiry_ts_ms,
                strike,
                option_right: option_right.map(Into::into),
                contract_multiplier,
            };
            let order = NormalizedOrderRequest {
                venue,
                symbol,
                instrument,
                strategy_id,
                client_order_id,
                intent_id,
                side: side.into(),
                order_type: order_type.into(),
                qty,
                limit_price,
                tif: tif.map(Into::into),
                post_only,
                reduce_only,
                requested_notional_cents,
            };
            let payload = ExecutionPlacePayload {
                order,
                approval_token,
            };

            (
                ExecutionCommand::Place.as_kind(),
                serde_json::to_value(payload)?,
            )
        }
        Commands::ExecutionCancel { venue_order_id } => {
            let payload = ExecutionCancelPayload { venue_order_id };
            (
                ExecutionCommand::Cancel.as_kind(),
                serde_json::to_value(payload)?,
            )
        }
        Commands::ExecutionGet { venue_order_id } => {
            let payload = ExecutionGetPayload { venue_order_id };
            (
                ExecutionCommand::Get.as_kind(),
                serde_json::to_value(payload)?,
            )
        }
        Commands::ExecutionOpenOrders => (
            ExecutionCommand::OpenOrders.as_kind(),
            serde_json::json!({}),
        ),
        Commands::ExecutionFills { since_ts_ms, limit } => {
            let payload = ExecutionFillsPayload {
                since_ts_ms,
                limit: Some(limit),
            };
            (
                ExecutionCommand::Fills.as_kind(),
                serde_json::to_value(payload)?,
            )
        }
        Commands::PortfolioPositions => {
            (PortfolioCommand::Positions.as_kind(), serde_json::json!({}))
        }
        Commands::PortfolioBalances => {
            (PortfolioCommand::Balances.as_kind(), serde_json::json!({}))
        }
        Commands::PortfolioExposure => {
            (PortfolioCommand::Exposure.as_kind(), serde_json::json!({}))
        }
        Commands::Raw { json } => ("Control.Raw", serde_json::from_str(&json)?),
    };

    let envelope = Envelope::new(kind, payload);
    let bytes = serde_json::to_vec(&envelope)?;

    framed.send(bytes.into()).await?;

    if let Some(resp) = framed.next().await {
        let resp_bytes = resp?;
        let resp_env: Envelope = serde_json::from_slice(&resp_bytes)?;
        println!("{}", serde_json::to_string_pretty(&resp_env)?);
    } else {
        println!("No response received.");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse_risk_override_value;
    use serde_json::json;

    #[test]
    fn parse_risk_override_value_parses_json() {
        let parsed = parse_risk_override_value(r#"{"enabled":true}"#);
        assert_eq!(parsed, json!({ "enabled": true }));
    }

    #[test]
    fn parse_risk_override_value_falls_back_to_string() {
        let parsed = parse_risk_override_value("{");
        assert_eq!(parsed, serde_json::Value::String("{".to_string()));
    }
}
