use anyhow::{Context, Result};
use bytes::Bytes;
use fs2::FileExt;
use futures::{SinkExt, StreamExt};
use risk_core::{HardSafetyCage, HardSafetyPolicy, PromotionRequest, RiskDecision, RiskSnapshot};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::fs::File;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use strategy_core::StrategyFamily;
use tokio::net::{UnixListener, UnixStream};
use tokio::signal;
use tokio::sync::Mutex;
use tokio_util::codec::Framed;
use tracing::{error, info, warn};
use trading_protocol::{
    create_codec, CandidatePromotePayload, CandidateUploadPayload, ControlCommand, EngineCommand,
    EngineStatePayload, Envelope, Event, RequestKind, RiskCommand, RiskLimitsPayload,
    RiskOverridePayload, RiskStatePayload, StrategyCommand, StrategySummaryPayload,
    DEFAULT_SOCKET_PATH,
};

const DEFAULT_LOCK_PATH: &str = "/var/run/openclaw/trading.lock";
const MAX_RECENT_EVENTS: usize = 128;

#[derive(Debug, Clone)]
struct StrategyCandidate {
    source: String,
    code_hash: String,
    requested_canary_notional_cents: i64,
    compile_passed: bool,
    replay_passed: bool,
    paper_passed: bool,
    latency_passed: bool,
    risk_passed: bool,
    uploaded_at_ms: i64,
}

impl StrategyCandidate {
    fn all_gates_passed(&self) -> bool {
        self.compile_passed
            && self.replay_passed
            && self.paper_passed
            && self.latency_passed
            && self.risk_passed
    }
}

#[derive(Debug, Clone)]
struct StrategyState {
    id: String,
    enabled: bool,
    family: StrategyFamily,
    source: String,
    version: u64,
    canary_deployment: bool,
    canary_notional_cents: i64,
    active_code_hash: Option<String>,
    candidate: Option<StrategyCandidate>,
}

impl StrategyState {
    fn summary(&self) -> StrategySummaryPayload {
        StrategySummaryPayload {
            id: self.id.clone(),
            enabled: self.enabled,
            family: format!("{:?}", self.family),
            source: self.source.clone(),
            version: self.version,
            canary_deployment: self.canary_deployment,
            canary_notional_cents: self.canary_notional_cents,
            active_code_hash: self.active_code_hash.clone(),
        }
    }
}

#[derive(Debug, Default)]
struct EngineState {
    running: bool,
    paused: bool,
    kill_switch_engaged: bool,
    risk_tripped: bool,
    started_at_ms: i64,
    last_command_at_ms: i64,
    strategies: HashMap<String, StrategyState>,
    recent_events: VecDeque<serde_json::Value>,
    risk_snapshot: RiskSnapshot,
    safety_policy: HardSafetyPolicy,
}

#[derive(Debug, Deserialize)]
struct StrategyIdPayload {
    strategy_id: String,
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
    let state = Arc::new(Mutex::new(initial_engine_state()));

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

fn initial_engine_state() -> EngineState {
    let now = now_ms();
    EngineState {
        running: false,
        paused: false,
        kill_switch_engaged: false,
        risk_tripped: false,
        started_at_ms: now,
        last_command_at_ms: now,
        strategies: default_strategies(),
        recent_events: VecDeque::new(),
        risk_snapshot: RiskSnapshot::default(),
        safety_policy: HardSafetyPolicy::default(),
    }
}

fn default_strategies() -> HashMap<String, StrategyState> {
    [
        (
            "kalshi.arbitrage",
            StrategyFamily::Arbitrage,
            "builtin-kalshi-arb",
        ),
        (
            "kalshi.market_making",
            StrategyFamily::MarketMaking,
            "builtin-kalshi-mm",
        ),
        (
            "core.mean_reversion",
            StrategyFamily::MeanReversion,
            "builtin-mean-reversion",
        ),
        (
            "core.momentum",
            StrategyFamily::Momentum,
            "builtin-momentum",
        ),
    ]
    .into_iter()
    .map(|(id, family, source)| {
        (
            id.to_string(),
            StrategyState {
                id: id.to_string(),
                enabled: true,
                family,
                source: source.to_string(),
                version: 1,
                canary_deployment: false,
                canary_notional_cents: 0,
                active_code_hash: None,
                candidate: None,
            },
        )
    })
    .collect()
}

fn now_ms() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(dur) => dur.as_millis() as i64,
        Err(_) => 0,
    }
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
    // Containers run as uid/gid 1000, so owner/group write access is sufficient.
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
    match RequestKind::from_kind(request.kind.as_str()) {
        Some(RequestKind::Control(command)) => {
            process_control_request(request, state, command).await
        }
        Some(RequestKind::Engine(command)) => process_engine_request(request, state, command).await,
        Some(RequestKind::Strategy(command)) => {
            process_strategy_request(request, state, command).await
        }
        Some(RequestKind::Risk(command)) => process_risk_request(request, state, command).await,
        None => Envelope::response_to(
            request,
            json!({
                "ok": false,
                "error": format!("Unsupported command kind '{}'", request.kind)
            }),
        ),
    }
}

async fn process_control_request(
    request: &Envelope,
    state: &Arc<Mutex<EngineState>>,
    command: ControlCommand,
) -> Envelope {
    match command {
        ControlCommand::Ping => Envelope::response_to(
            request,
            json!({
                "ok": true,
                "connected": true,
            }),
        ),
        ControlCommand::Status => {
            let state = state.lock().await;
            status_response(request, &state)
        }
        ControlCommand::Start => {
            let mut state = state.lock().await;
            if state.kill_switch_engaged {
                return Envelope::response_to(
                    request,
                    json!({
                        "ok": false,
                        "error": "Kill switch engaged; cannot start until operator reset"
                    }),
                );
            }

            state.running = true;
            state.paused = false;
            state.last_command_at_ms = now_ms();
            let event = engine_health_event(&state);
            push_event(&mut state, event);
            status_response(request, &state)
        }
        ControlCommand::Stop => {
            let mut state = state.lock().await;
            state.running = false;
            state.paused = true;
            state.last_command_at_ms = now_ms();
            let event = engine_health_event(&state);
            push_event(&mut state, event);
            status_response(request, &state)
        }
    }
}

async fn process_engine_request(
    request: &Envelope,
    state: &Arc<Mutex<EngineState>>,
    command: EngineCommand,
) -> Envelope {
    match command {
        EngineCommand::Status => {
            let state = state.lock().await;
            status_response(request, &state)
        }
        EngineCommand::Pause => {
            let mut state = state.lock().await;
            state.paused = true;
            state.running = false;
            state.risk_snapshot.paused = true;
            state.last_command_at_ms = now_ms();
            let event = engine_health_event(&state);
            push_event(&mut state, event);
            status_response(request, &state)
        }
        EngineCommand::Resume => {
            let mut state = state.lock().await;
            if state.kill_switch_engaged {
                return Envelope::response_to(
                    request,
                    json!({
                        "ok": false,
                        "error": "Kill switch engaged; cannot resume"
                    }),
                );
            }
            state.paused = false;
            state.running = true;
            state.risk_snapshot.paused = false;
            state.last_command_at_ms = now_ms();
            let event = engine_health_event(&state);
            push_event(&mut state, event);
            status_response(request, &state)
        }
        EngineCommand::KillSwitch => {
            let mut state = state.lock().await;
            state.kill_switch_engaged = true;
            state.risk_tripped = true;
            state.running = false;
            state.paused = true;
            state.risk_snapshot.kill_switch_engaged = true;
            state.risk_snapshot.paused = true;
            state.last_command_at_ms = now_ms();

            push_event(
                &mut state,
                Event::RiskAlert {
                    level: "critical".to_string(),
                    reason: "manual_kill_switch".to_string(),
                    kill_switch_engaged: true,
                },
            );
            let event = engine_health_event(&state);
            push_event(&mut state, event);
            status_response(request, &state)
        }
    }
}

async fn process_strategy_request(
    request: &Envelope,
    state: &Arc<Mutex<EngineState>>,
    command: StrategyCommand,
) -> Envelope {
    match command {
        StrategyCommand::List => {
            let state = state.lock().await;
            Envelope::response_to(
                request,
                json!({
                    "ok": true,
                    "strategies": strategy_summaries(&state),
                }),
            )
        }
        StrategyCommand::Enable | StrategyCommand::Disable => {
            let payload: StrategyIdPayload = match parse_payload(&request.payload) {
                Ok(p) => p,
                Err(err) => {
                    return Envelope::response_to(request, json!({"ok": false, "error": err}));
                }
            };

            let mut state = state.lock().await;
            let enabled = matches!(command, StrategyCommand::Enable);
            let (summary, event) = {
                let strategy = match state.strategies.get_mut(&payload.strategy_id) {
                    Some(strategy) => strategy,
                    None => {
                        return Envelope::response_to(
                            request,
                            json!({
                                "ok": false,
                                "error": format!("Unknown strategy '{}'", payload.strategy_id)
                            }),
                        );
                    }
                };

                strategy.enabled = enabled;
                let phase = if enabled { "enabled" } else { "disabled" };
                let event = Event::StrategyLifecycle {
                    strategy_id: strategy.id.clone(),
                    phase: phase.to_string(),
                    version: strategy.version,
                    code_hash: strategy.active_code_hash.clone(),
                };
                (strategy.summary(), event)
            };

            state.last_command_at_ms = now_ms();
            push_event(&mut state, event);

            Envelope::response_to(
                request,
                json!({
                    "ok": true,
                    "strategy": summary,
                }),
            )
        }
        StrategyCommand::UploadCandidate => {
            let payload: CandidateUploadPayload = match parse_payload(&request.payload) {
                Ok(p) => p,
                Err(err) => {
                    return Envelope::response_to(request, json!({"ok": false, "error": err}));
                }
            };

            let mut state = state.lock().await;
            let candidate = StrategyCandidate {
                source: payload.source.clone(),
                code_hash: payload.code_hash.clone(),
                requested_canary_notional_cents: payload.requested_canary_notional_cents,
                compile_passed: payload.compile_passed,
                replay_passed: payload.replay_passed,
                paper_passed: payload.paper_passed,
                latency_passed: payload.latency_passed,
                risk_passed: payload.risk_passed,
                uploaded_at_ms: now_ms(),
            };

            let candidate_ok = candidate.all_gates_passed();
            let (strategy_id, event) = {
                let strategy = match state.strategies.get_mut(&payload.strategy_id) {
                    Some(strategy) => strategy,
                    None => {
                        return Envelope::response_to(
                            request,
                            json!({
                                "ok": false,
                                "error": format!("Unknown strategy '{}'", payload.strategy_id)
                            }),
                        );
                    }
                };

                strategy.candidate = Some(candidate.clone());
                let event = Event::AgentCodegen {
                    strategy_id: strategy.id.clone(),
                    phase: "uploaded".to_string(),
                    code_hash: candidate.code_hash.clone(),
                    passed: candidate_ok,
                    reason: if candidate_ok {
                        None
                    } else {
                        Some("candidate uploaded but at least one gate failed".to_string())
                    },
                };
                (strategy.id.clone(), event)
            };

            state.last_command_at_ms = now_ms();
            push_event(&mut state, event);

            Envelope::response_to(
                request,
                json!({
                    "ok": true,
                    "strategy_id": strategy_id,
                    "candidate": {
                        "source": candidate.source,
                        "code_hash": candidate.code_hash,
                        "requested_canary_notional_cents": candidate.requested_canary_notional_cents,
                        "all_gates_passed": candidate_ok,
                        "uploaded_at_ms": candidate.uploaded_at_ms,
                    }
                }),
            )
        }
        StrategyCommand::PromoteCandidate => {
            let payload: CandidatePromotePayload = match parse_payload(&request.payload) {
                Ok(p) => p,
                Err(err) => {
                    return Envelope::response_to(request, json!({"ok": false, "error": err}));
                }
            };

            let mut state = state.lock().await;
            let (strategy_id, version, candidate) = {
                let strategy = match state.strategies.get(&payload.strategy_id) {
                    Some(strategy) => strategy,
                    None => {
                        return Envelope::response_to(
                            request,
                            json!({
                                "ok": false,
                                "error": format!("Unknown strategy '{}'", payload.strategy_id)
                            }),
                        );
                    }
                };

                let candidate = match strategy.candidate.clone() {
                    Some(candidate) => candidate,
                    None => {
                        return Envelope::response_to(
                            request,
                            json!({
                                "ok": false,
                                "error": format!("No uploaded candidate for '{}'", payload.strategy_id)
                            }),
                        );
                    }
                };

                (strategy.id.clone(), strategy.version, candidate)
            };

            if candidate.code_hash != payload.code_hash {
                return Envelope::response_to(
                    request,
                    json!({
                        "ok": false,
                        "error": "candidate code hash mismatch"
                    }),
                );
            }

            let promote_req = PromotionRequest {
                strategy_id: strategy_id.clone(),
                code_hash: payload.code_hash.clone(),
                requested_canary_notional_cents: payload.requested_canary_notional_cents,
                compile_passed: candidate.compile_passed,
                replay_passed: candidate.replay_passed,
                paper_passed: candidate.paper_passed,
                latency_passed: candidate.latency_passed,
                risk_passed: candidate.risk_passed,
            };

            let cage = HardSafetyCage::new(state.safety_policy.clone());
            match cage.evaluate_promotion(&promote_req, &state.risk_snapshot) {
                RiskDecision::Allow => {
                    let phase = if payload.auto {
                        "promoted_auto"
                    } else {
                        "promoted_manual"
                    }
                    .to_string();
                    let (summary, promoted_version, promoted_code_hash) = {
                        let strategy = state
                            .strategies
                            .get_mut(&strategy_id)
                            .expect("strategy must exist");
                        strategy.version = strategy.version.saturating_add(1);
                        strategy.enabled = true;
                        strategy.canary_deployment = true;
                        strategy.canary_notional_cents = payload.requested_canary_notional_cents;
                        strategy.active_code_hash = Some(payload.code_hash.clone());
                        strategy.source = candidate.source;
                        strategy.candidate = None;

                        (
                            strategy.summary(),
                            strategy.version,
                            strategy.active_code_hash.clone(),
                        )
                    };

                    state
                        .risk_snapshot
                        .strategy_canary_notional
                        .insert(strategy_id.clone(), payload.requested_canary_notional_cents);
                    state.last_command_at_ms = now_ms();

                    push_event(
                        &mut state,
                        Event::StrategyLifecycle {
                            strategy_id: strategy_id.clone(),
                            phase,
                            version: promoted_version,
                            code_hash: promoted_code_hash,
                        },
                    );

                    Envelope::response_to(
                        request,
                        json!({
                            "ok": true,
                            "strategy": summary,
                            "previous_version": version,
                            "hard_safety_floor": "enforced",
                        }),
                    )
                }
                RiskDecision::Deny { reason } => {
                    warn!(
                        "Candidate promotion denied for {} ({}): {}",
                        strategy_id, payload.code_hash, reason
                    );

                    let kill_switch_engaged = state.kill_switch_engaged;
                    push_event(
                        &mut state,
                        Event::RiskAlert {
                            level: "warning".to_string(),
                            reason: format!(
                                "promotion denied for {}:{} ({})",
                                strategy_id, payload.code_hash, reason
                            ),
                            kill_switch_engaged,
                        },
                    );

                    Envelope::response_to(
                        request,
                        json!({
                            "ok": false,
                            "error": reason,
                            "hard_safety_floor": "enforced",
                        }),
                    )
                }
            }
        }
    }
}

async fn process_risk_request(
    request: &Envelope,
    state: &Arc<Mutex<EngineState>>,
    command: RiskCommand,
) -> Envelope {
    match command {
        RiskCommand::Status => {
            let state = state.lock().await;
            Envelope::response_to(
                request,
                json!({
                    "ok": true,
                    "safety_floor": "hard_cage",
                    "limits": risk_limits_payload(&state),
                    "state": risk_state_payload(&state),
                }),
            )
        }
        RiskCommand::Override => {
            let payload: RiskOverridePayload = match parse_payload(&request.payload) {
                Ok(p) => p,
                Err(err) => {
                    return Envelope::response_to(request, json!({"ok": false, "error": err}));
                }
            };

            let mut state = state.lock().await;
            if payload.action == "clear_runtime_counters" {
                state.risk_snapshot.orders_last_minute = 0;
                state.last_command_at_ms = now_ms();
                return Envelope::response_to(
                    request,
                    json!({
                        "ok": true,
                        "action": payload.action,
                        "state": risk_state_payload(&state),
                    }),
                );
            }

            Envelope::response_to(
                request,
                json!({
                    "ok": false,
                    "error": "hard safety floor cannot be overridden",
                    "action": payload.action,
                }),
            )
        }
    }
}

fn parse_payload<T>(payload: &serde_json::Value) -> std::result::Result<T, String>
where
    T: DeserializeOwned,
{
    serde_json::from_value(payload.clone()).map_err(|e| format!("Invalid payload: {}", e))
}

fn strategy_summaries(state: &EngineState) -> Vec<StrategySummaryPayload> {
    let mut summaries: Vec<_> = state
        .strategies
        .values()
        .map(StrategyState::summary)
        .collect();
    summaries.sort_by(|a, b| a.id.cmp(&b.id));
    summaries
}

fn risk_limits_payload(state: &EngineState) -> RiskLimitsPayload {
    RiskLimitsPayload {
        max_total_notional_cents: state.safety_policy.max_total_notional_cents,
        max_strategy_canary_notional_cents: state.safety_policy.max_strategy_canary_notional_cents,
        max_orders_per_minute: state.safety_policy.max_orders_per_minute,
        max_drawdown_cents: state.safety_policy.max_drawdown_cents,
        forced_cooldown_secs: state.safety_policy.forced_cooldown_secs,
    }
}

fn risk_state_payload(state: &EngineState) -> RiskStatePayload {
    RiskStatePayload {
        kill_switch_engaged: state.kill_switch_engaged,
        paused: state.paused,
        orders_last_minute: state.risk_snapshot.orders_last_minute,
        drawdown_cents: state.risk_snapshot.drawdown_cents,
        total_notional_cents: state.risk_snapshot.total_notional_cents,
    }
}

fn engine_state_payload(state: &EngineState) -> EngineStatePayload {
    let strategies_enabled = state.strategies.values().filter(|s| s.enabled).count();

    EngineStatePayload {
        running: state.running,
        paused: state.paused,
        kill_switch_engaged: state.kill_switch_engaged,
        risk_tripped: state.risk_tripped,
        started_at_ms: state.started_at_ms,
        last_command_at_ms: state.last_command_at_ms,
        strategies_total: state.strategies.len(),
        strategies_enabled,
    }
}

fn engine_health_event(state: &EngineState) -> Event {
    Event::EngineHealth {
        running: state.running,
        paused: state.paused,
        kill_switch_engaged: state.kill_switch_engaged,
        risk_tripped: state.risk_tripped,
    }
}

fn status_response(request: &Envelope, state: &EngineState) -> Envelope {
    let recent_events: Vec<_> = state.recent_events.iter().cloned().collect();
    Envelope::response_to(
        request,
        json!({
            "ok": true,
            "state": engine_state_payload(state),
            "risk": {
                "limits": risk_limits_payload(state),
                "state": risk_state_payload(state),
            },
            "strategies": strategy_summaries(state),
            "recent_events": recent_events,
        }),
    )
}

fn push_event(state: &mut EngineState, event: Event) {
    let value = match serde_json::to_value(event) {
        Ok(v) => v,
        Err(e) => {
            warn!("Failed to serialize event: {}", e);
            return;
        }
    };

    state.recent_events.push_back(value);
    while state.recent_events.len() > MAX_RECENT_EVENTS {
        state.recent_events.pop_front();
    }
}
