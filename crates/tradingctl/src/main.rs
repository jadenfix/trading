use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use futures::{SinkExt, StreamExt};
use tokio::net::UnixStream;
use tokio_util::codec::Framed;
use trading_protocol::{
    create_codec, CandidatePromotePayload, CandidateUploadPayload, ControlCommand, EngineCommand,
    Envelope, RiskCommand, RiskOverridePayload, StrategyCommand, DEFAULT_SOCKET_PATH,
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
