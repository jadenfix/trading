use anyhow::{Context, Result};
use bytes::Bytes;
use fs2::FileExt;
use futures::{SinkExt, StreamExt};
use serde_json::json;
use std::fs::File;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::net::{UnixListener, UnixStream};
use tokio::signal;
use tokio::sync::Mutex;
use tokio_util::codec::Framed;
use tracing::{error, info};
use trading_protocol::{create_codec, ControlCommand, Envelope, DEFAULT_SOCKET_PATH};

const DEFAULT_LOCK_PATH: &str = "/var/run/openclaw/trading.lock";

#[derive(Debug, Default)]
struct EngineState {
    running: bool,
    risk_tripped: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let socket_path = socket_path_from_env();
    let lock_path = lock_path_from_env();
    info!("Starting trading daemon");
    info!("Socket path: {}", socket_path);
    info!("Lock path: {}", lock_path);

    ensure_socket_parent_dir(&socket_path)?;
    let _lock_file = acquire_single_instance_lock(&lock_path)?;
    let listener = bind_listener(&socket_path)?;
    let state = Arc::new(Mutex::new(EngineState::default()));

    let terminate = signal::ctrl_c();
    tokio::pin!(terminate);

    loop {
        tokio::select! {
             _ = &mut terminate => {
                info!("Shutdown signal received");
                break;
            }
            res = listener.accept() => {
                match res {
                     Ok((stream, _addr)) => {
                        let state = Arc::clone(&state);
                        tokio::spawn(async move {
                            handle_connection(stream, state).await;
                        });
                     }
                     Err(e) => {
                         error!("Accept error: {:?}", e);
                     }
                }
            }
        }
    }

    if Path::new(&socket_path).exists() {
        if let Err(err) = std::fs::remove_file(&socket_path) {
            error!("Failed to clean up socket {}: {:?}", socket_path, err);
        }
    }
    info!("Trading daemon stopped");

    Ok(())
}

fn socket_path_from_env() -> String {
    std::env::var("TRADING_SOCKET_PATH").unwrap_or_else(|_| DEFAULT_SOCKET_PATH.to_string())
}

fn lock_path_from_env() -> String {
    std::env::var("TRADING_LOCK_PATH").unwrap_or_else(|_| DEFAULT_LOCK_PATH.to_string())
}

fn ensure_socket_parent_dir(socket_path: &str) -> Result<()> {
    let parent = PathBuf::from(socket_path)
        .parent()
        .map(Path::to_path_buf)
        .context("Socket path must include a parent directory")?;
    std::fs::create_dir_all(&parent)
        .with_context(|| format!("Failed to create socket directory {}", parent.display()))?;
    Ok(())
}

fn acquire_single_instance_lock(lock_path: &str) -> Result<File> {
    if let Some(parent) = Path::new(lock_path).parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create lock directory {}", parent.display()))?;
    }

    let lock_file = File::create(lock_path)
        .with_context(|| format!("Failed to create lock file {}", lock_path))?;
    lock_file
        .try_lock_exclusive()
        .with_context(|| format!("Another daemon instance is already running ({})", lock_path))?;
    info!("Acquired lock on {}", lock_path);
    Ok(lock_file)
}

fn bind_listener(socket_path: &str) -> Result<UnixListener> {
    if Path::new(socket_path).exists() {
        std::fs::remove_file(socket_path)
            .with_context(|| format!("Failed to remove stale socket {}", socket_path))?;
    }
    let listener = UnixListener::bind(socket_path)
        .with_context(|| format!("Failed to bind UDS socket {}", socket_path))?;
    #[cfg(unix)]
    std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o660))
        .with_context(|| format!("Failed to set socket permissions on {}", socket_path))?;
    info!("Listening on {}", socket_path);
    Ok(listener)
}

async fn handle_connection(stream: UnixStream, state: Arc<Mutex<EngineState>>) {
    let mut framed = Framed::new(stream, create_codec());

    while let Some(result) = framed.next().await {
        match result {
            Ok(bytes) => {
                let envelope: Envelope = match serde_json::from_slice(&bytes) {
                    Ok(envelope) => envelope,
                    Err(e) => {
                        error!("Invalid envelope payload: {:?}", e);
                        continue;
                    }
                };

                info!("Received request: {}", envelope.kind);
                let response = process_request(&envelope, &state).await;
                let response_bytes = match serde_json::to_vec(&response) {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        error!("Failed to encode response: {:?}", e);
                        continue;
                    }
                };

                if let Err(e) = framed.send(Bytes::from(response_bytes)).await {
                    error!("Failed to send response: {:?}", e);
                    break;
                }
            }
            Err(e) => {
                error!("Codec error: {:?}", e);
                break;
            }
        }
    }
}

async fn process_request(request: &Envelope, state: &Arc<Mutex<EngineState>>) -> Envelope {
    match ControlCommand::from_kind(request.kind.as_str()) {
        Some(ControlCommand::Ping) => Envelope::response_to(
            request,
            json!({
                "ok": true,
                "connected": true
            }),
        ),
        Some(ControlCommand::Status) => {
            let state = state.lock().await;
            Envelope::response_to(
                request,
                json!({
                    "ok": true,
                    "state": {
                        "running": state.running,
                        "risk_tripped": state.risk_tripped
                    }
                }),
            )
        }
        Some(ControlCommand::Start) => {
            let mut state = state.lock().await;
            state.running = true;
            Envelope::response_to(
                request,
                json!({
                    "ok": true,
                    "state": {
                        "running": state.running,
                        "risk_tripped": state.risk_tripped
                    }
                }),
            )
        }
        Some(ControlCommand::Stop) => {
            let mut state = state.lock().await;
            state.running = false;
            Envelope::response_to(
                request,
                json!({
                    "ok": true,
                    "state": {
                        "running": state.running,
                        "risk_tripped": state.risk_tripped
                    }
                }),
            )
        }
        None => Envelope::response_to(
            request,
            json!({
                "ok": false,
                "error": format!("Unsupported command kind '{}'", request.kind)
            }),
        ),
    }
}
