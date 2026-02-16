use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use futures::{SinkExt, StreamExt};
use tokio::net::UnixStream;
use tokio_util::codec::Framed;
use trading_protocol::{create_codec, ControlCommand, Envelope, DEFAULT_SOCKET_PATH};

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
    /// Check daemon status
    Status,
    /// Send a raw JSON command
    Raw {
        #[arg(short, long)]
        json: String,
    },
    /// Send Control.Start command
    Start,
    /// Send Control.Stop command
    Stop,
    /// Send Control.Ping command
    Ping,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let stream = UnixStream::connect(&cli.socket)
        .await
        .context("Failed to connect to daemon socket. Is the daemon running?")?;

    let mut framed = Framed::new(stream, create_codec());

    let (kind, payload) = match cli.command {
        Commands::Status => (ControlCommand::Status.as_kind(), serde_json::json!({})),
        Commands::Start => (ControlCommand::Start.as_kind(), serde_json::json!({})),
        Commands::Stop => (ControlCommand::Stop.as_kind(), serde_json::json!({})),
        Commands::Ping => (ControlCommand::Ping.as_kind(), serde_json::json!({})),
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
