use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use futures::{SinkExt, StreamExt};
use tokio::net::UnixStream;
use tokio_util::codec::Framed;
use trading_protocol::{
    create_codec, CandidatePromotePayload, CandidateUploadPayload, ControlCommand, EngineCommand,
    Envelope, ExecutionModeCommand, ExecutionModePayload, InstrumentRefPayload, OrderCancelPayload,
    OrderCommand, OrderSubmitPayload, PortfolioCommand, RiskCommand, RiskOverridePayload,
    StrategyCommand, VenueCommand, VenueTogglePayload, DEFAULT_SOCKET_PATH,
};

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
    },
    /// Send Venue.List command
    VenueList,
    /// Send Venue.Enable command
    VenueEnable {
        #[arg(long)]
        venue_id: String,
    },
    /// Send Venue.Disable command
    VenueDisable {
        #[arg(long)]
        venue_id: String,
    },
    /// Send Venue.Status command
    VenueStatus {
        #[arg(long)]
        venue_id: String,
    },
    /// Send Portfolio.Balances command
    PortfolioBalances,
    /// Send Portfolio.Positions command
    PortfolioPositions,
    /// Send Order.Submit command
    OrderSubmit {
        #[arg(long)]
        strategy_id: String,
        #[arg(long)]
        venue_id: String,
        #[arg(long)]
        symbol: String,
        #[arg(long)]
        asset_class: String,
        #[arg(long)]
        market_type: String,
        #[arg(long)]
        side: String,
        #[arg(long)]
        order_type: String,
        #[arg(long)]
        quantity: f64,
        #[arg(long)]
        limit_price: Option<f64>,
        #[arg(long)]
        tif: Option<String>,
        #[arg(long, default_value_t = false)]
        post_only: bool,
        #[arg(long, default_value_t = false)]
        reduce_only: bool,
        #[arg(long)]
        client_order_id: String,
    },
    /// Send Order.Cancel command
    OrderCancel {
        #[arg(long)]
        venue_order_id: String,
        #[arg(long)]
        venue_id: Option<String>,
    },
    /// Send Order.List command
    OrderList {
        #[arg(long)]
        venue_id: Option<String>,
    },
    /// Send ExecutionMode.Get command
    ExecutionModeGet {
        #[arg(long)]
        venue_id: String,
        #[arg(long)]
        market_type: String,
    },
    /// Send ExecutionMode.Set command
    ExecutionModeSet {
        #[arg(long)]
        venue_id: String,
        #[arg(long)]
        market_type: String,
        #[arg(long)]
        mode: String,
    },
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
        Commands::RiskOverride { action, value } => {
            let parsed_value = value.as_ref().map(|raw| parse_risk_override_value(raw));

            let payload = RiskOverridePayload {
                action,
                value: parsed_value,
            };
            (
                RiskCommand::Override.as_kind(),
                serde_json::to_value(payload)?,
            )
        }
        Commands::VenueList => (VenueCommand::List.as_kind(), serde_json::json!({})),
        Commands::VenueEnable { venue_id } => (
            VenueCommand::Enable.as_kind(),
            serde_json::to_value(VenueTogglePayload { venue_id })?,
        ),
        Commands::VenueDisable { venue_id } => (
            VenueCommand::Disable.as_kind(),
            serde_json::to_value(VenueTogglePayload { venue_id })?,
        ),
        Commands::VenueStatus { venue_id } => (
            VenueCommand::Status.as_kind(),
            serde_json::to_value(VenueTogglePayload { venue_id })?,
        ),
        Commands::PortfolioBalances => {
            (PortfolioCommand::Balances.as_kind(), serde_json::json!({}))
        }
        Commands::PortfolioPositions => {
            (PortfolioCommand::Positions.as_kind(), serde_json::json!({}))
        }
        Commands::OrderSubmit {
            strategy_id,
            venue_id,
            symbol,
            asset_class,
            market_type,
            side,
            order_type,
            quantity,
            limit_price,
            tif,
            post_only,
            reduce_only,
            client_order_id,
        } => {
            let payload = OrderSubmitPayload {
                strategy_id,
                venue_id: venue_id.clone(),
                instrument: InstrumentRefPayload {
                    venue_id,
                    symbol,
                    asset_class,
                    market_type,
                    expiry_ts_ms: None,
                    strike: None,
                    option_type: None,
                },
                side,
                order_type,
                quantity,
                limit_price,
                tif,
                post_only,
                reduce_only,
                client_order_id,
            };
            (
                OrderCommand::Submit.as_kind(),
                serde_json::to_value(payload)?,
            )
        }
        Commands::OrderCancel {
            venue_order_id,
            venue_id,
        } => (
            OrderCommand::Cancel.as_kind(),
            serde_json::to_value(OrderCancelPayload {
                venue_id,
                venue_order_id,
            })?,
        ),
        Commands::OrderList { venue_id } => (
            OrderCommand::List.as_kind(),
            serde_json::json!({ "venue_id": venue_id }),
        ),
        Commands::ExecutionModeGet {
            venue_id,
            market_type,
        } => (
            ExecutionModeCommand::Get.as_kind(),
            serde_json::to_value(ExecutionModePayload {
                venue_id,
                market_type,
                mode: String::new(),
            })?,
        ),
        Commands::ExecutionModeSet {
            venue_id,
            market_type,
            mode,
        } => (
            ExecutionModeCommand::Set.as_kind(),
            serde_json::to_value(ExecutionModePayload {
                venue_id,
                market_type,
                mode,
            })?,
        ),
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
