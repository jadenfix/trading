use anyhow::{Context, Result};
use bytes::Bytes;
use fs2::FileExt;
use futures::{SinkExt, StreamExt};
use risk_core::{HardSafetyCage, HardSafetyPolicy, PromotionRequest, RiskDecision, RiskSnapshot};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::ErrorKind;
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
    create_codec, CandidatePromotePayload, CandidateUploadPayload, CapabilitiesPayload,
    ControlCommand, DaemonBuildPayload, EngineCommand, EngineStatePayload, Envelope, Event,
    RequestKind, RiskCommand, RiskLimitsPayload, RiskOverridePayload, RiskStatePayload,
    StrategyCommand, StrategySummaryPayload, DEFAULT_SOCKET_PATH, PROTOCOL_VERSION,
    STATUS_SCHEMA_VERSION,
};

const DEFAULT_LOCK_PATH: &str = "/var/run/openclaw/trading.lock";
const DEFAULT_STATE_PATH: &str = "/var/run/openclaw/trading-state.json";
const DEFAULT_CANDIDATE_TTL_MS: i64 = 0;
const MAX_RECENT_EVENTS: usize = 128;

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    state_path: String,
    candidate_ttl_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StrategyStateSnapshot {
    schema_version: u8,
    saved_at_ms: i64,
    strategies: Vec<StrategyState>,
}

#[derive(Debug)]
struct PromotionSuccess {
    strategy_id: String,
    previous_version: u64,
    summary: StrategySummaryPayload,
    promoted_version: u64,
    promoted_code_hash: Option<String>,
    phase: String,
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
    let state_path = state_path_from_env();
    let candidate_ttl_ms = candidate_ttl_ms_from_env();
    info!("Starting trading daemon");
    info!("Socket path: {}", socket_path);
    info!("Lock path: {}", lock_path);
    info!("State path: {}", state_path);
    info!("Candidate TTL (ms): {}", candidate_ttl_ms);

    ensure_socket_parent_dir(&socket_path)?;
    let _lock_file = acquire_single_instance_lock(&lock_path)?;
    let listener = bind_listener(&socket_path)?;
    let state = Arc::new(Mutex::new(initial_engine_state(
        state_path,
        candidate_ttl_ms,
    )));

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

fn initial_engine_state(state_path: String, candidate_ttl_ms: i64) -> EngineState {
    let now = now_ms();
    let mut state = EngineState {
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
        state_path,
        candidate_ttl_ms,
    };

    match load_strategy_snapshot(&state.state_path) {
        Ok(Some(snapshot)) => {
            apply_strategy_snapshot(&mut state, snapshot);
            info!("Loaded persisted strategy state from {}", state.state_path);
        }
        Ok(None) => {}
        Err(err) => {
            warn!(
                "Failed to load persisted strategy state from {}: {:#}",
                state.state_path, err
            );
        }
    }

    state
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
                enabled: false,
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

fn state_path_from_env() -> String {
    std::env::var("TRADING_STATE_PATH").unwrap_or_else(|_| DEFAULT_STATE_PATH.to_string())
}

fn candidate_ttl_ms_from_env() -> i64 {
    match std::env::var("TRADING_CANDIDATE_TTL_MS") {
        Ok(value) => match value.parse::<i64>() {
            Ok(parsed) if parsed >= 0 => parsed,
            _ => {
                warn!(
                    "Invalid TRADING_CANDIDATE_TTL_MS='{}'; defaulting to {}",
                    value, DEFAULT_CANDIDATE_TTL_MS
                );
                DEFAULT_CANDIDATE_TTL_MS
            }
        },
        Err(_) => DEFAULT_CANDIDATE_TTL_MS,
    }
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

fn strategy_snapshot_from_state(state: &EngineState) -> StrategyStateSnapshot {
    let mut strategies: Vec<StrategyState> = state.strategies.values().cloned().collect();
    strategies.sort_by(|a, b| a.id.cmp(&b.id));
    StrategyStateSnapshot {
        schema_version: 1,
        saved_at_ms: now_ms(),
        strategies,
    }
}

fn load_strategy_snapshot(path: &str) -> Result<Option<StrategyStateSnapshot>> {
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err).with_context(|| format!("Failed to read strategy snapshot {}", path));
        }
    };

    let snapshot: StrategyStateSnapshot = serde_json::from_slice(&raw)
        .with_context(|| format!("Failed to decode strategy snapshot {}", path))?;
    Ok(Some(snapshot))
}

fn save_strategy_snapshot(path: &str, snapshot: &StrategyStateSnapshot) -> Result<()> {
    if let Some(parent) = Path::new(path).parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "Failed to create parent directory for strategy snapshot {}",
                parent.display()
            )
        })?;
    }

    let tmp_path = format!("{}.tmp.{}", path, std::process::id());
    let bytes =
        serde_json::to_vec_pretty(snapshot).context("Failed to encode strategy snapshot")?;
    std::fs::write(&tmp_path, bytes)
        .with_context(|| format!("Failed to write temporary strategy snapshot {}", tmp_path))?;
    std::fs::rename(&tmp_path, path).with_context(|| {
        format!(
            "Failed to move temporary strategy snapshot {} to {}",
            tmp_path, path
        )
    })?;
    Ok(())
}

fn strategy_canary_notional_map(
    strategies: &HashMap<String, StrategyState>,
) -> HashMap<String, i64> {
    strategies
        .iter()
        .filter_map(|(id, strategy)| {
            if strategy.canary_deployment && strategy.canary_notional_cents > 0 {
                Some((id.clone(), strategy.canary_notional_cents))
            } else {
                None
            }
        })
        .collect()
}

fn apply_strategy_snapshot(state: &mut EngineState, snapshot: StrategyStateSnapshot) {
    for persisted in snapshot.strategies {
        let Some(strategy) = state.strategies.get_mut(&persisted.id) else {
            warn!(
                "Ignoring persisted strategy '{}' because it is not in defaults",
                persisted.id
            );
            continue;
        };

        if strategy.family != persisted.family {
            warn!(
                "Ignoring persisted strategy '{}' due to family mismatch ({:?} != {:?})",
                persisted.id, persisted.family, strategy.family
            );
            continue;
        }

        strategy.enabled = persisted.enabled;
        strategy.source = persisted.source;
        strategy.version = persisted.version.max(1);
        strategy.canary_deployment = persisted.canary_deployment;
        strategy.canary_notional_cents = persisted.canary_notional_cents.max(0);
        strategy.active_code_hash = persisted.active_code_hash;
        strategy.candidate = persisted.candidate;
    }

    state.risk_snapshot.strategy_canary_notional = strategy_canary_notional_map(&state.strategies);
}

fn persist_strategy_state(state: &EngineState) {
    let snapshot = strategy_snapshot_from_state(state);
    if let Err(err) = save_strategy_snapshot(&state.state_path, &snapshot) {
        warn!(
            "Failed to persist strategy state to {}: {:#}",
            state.state_path, err
        );
    }
}

fn set_engine_runtime_state(state: &mut EngineState, running: bool, paused: bool) {
    state.running = running;
    state.paused = paused;
    state.risk_snapshot.paused = paused;
}

fn apply_start(state: &mut EngineState) {
    set_engine_runtime_state(state, true, false);
    state.last_command_at_ms = now_ms();
}

fn apply_stop(state: &mut EngineState) {
    // Stop means halted but not "paused for safety"; resume requires explicit start.
    set_engine_runtime_state(state, false, false);
    state.last_command_at_ms = now_ms();
}

fn apply_pause(state: &mut EngineState) {
    set_engine_runtime_state(state, false, true);
    state.last_command_at_ms = now_ms();
}

fn apply_resume(state: &mut EngineState) {
    set_engine_runtime_state(state, true, false);
    state.last_command_at_ms = now_ms();
}

fn apply_kill_switch(state: &mut EngineState) {
    state.kill_switch_engaged = true;
    state.risk_tripped = true;
    set_engine_runtime_state(state, false, true);
    state.risk_snapshot.kill_switch_engaged = true;
    state.last_command_at_ms = now_ms();
}

fn apply_reset_kill_switch(state: &mut EngineState) {
    state.kill_switch_engaged = false;
    state.risk_snapshot.kill_switch_engaged = false;
    set_engine_runtime_state(state, false, true);
    state.last_command_at_ms = now_ms();
}

fn promote_candidate_locked(
    state: &mut EngineState,
    payload: &CandidatePromotePayload,
    promoted_at_ms: i64,
) -> std::result::Result<PromotionSuccess, String> {
    let (strategy_id, previous_version, candidate) = {
        let strategy = state
            .strategies
            .get(&payload.strategy_id)
            .ok_or_else(|| format!("Unknown strategy '{}'", payload.strategy_id))?;

        let candidate = strategy
            .candidate
            .clone()
            .ok_or_else(|| format!("No uploaded candidate for '{}'", payload.strategy_id))?;

        (strategy.id.clone(), strategy.version, candidate)
    };

    if candidate.code_hash != payload.code_hash {
        return Err("candidate code hash mismatch".to_string());
    }

    if state.candidate_ttl_ms > 0 {
        let age_ms = promoted_at_ms.saturating_sub(candidate.uploaded_at_ms);
        if age_ms > state.candidate_ttl_ms {
            return Err(format!(
                "candidate expired: age {}ms exceeds ttl {}ms",
                age_ms, state.candidate_ttl_ms
            ));
        }
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
                // Promotion replaces runtime code/canary config but should not override operator enable intent.
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
            state.last_command_at_ms = promoted_at_ms;

            Ok(PromotionSuccess {
                strategy_id,
                previous_version,
                summary,
                promoted_version,
                promoted_code_hash,
                phase,
            })
        }
        RiskDecision::Deny { reason } => Err(reason),
    }
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
            // Keep legacy Control.Status support for compatibility with older clients.
            // New clients should use Engine.Status.
            let state = state.lock().await;
            status_response(request, &state)
        }
        ControlCommand::Capabilities => Envelope::response_to(
            request,
            json!({
                "ok": true,
                "capabilities": capabilities_payload(),
            }),
        ),
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

            apply_start(&mut state);
            let event = engine_health_event(&state);
            push_event(&mut state, event);
            status_response(request, &state)
        }
        ControlCommand::Stop => {
            let mut state = state.lock().await;
            apply_stop(&mut state);
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
            apply_pause(&mut state);
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
            apply_resume(&mut state);
            let event = engine_health_event(&state);
            push_event(&mut state, event);
            status_response(request, &state)
        }
        EngineCommand::KillSwitch => {
            let mut state = state.lock().await;
            apply_kill_switch(&mut state);

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
            persist_strategy_state(&state);

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
            persist_strategy_state(&state);

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
            // Promotion checks and mutations are serialized by the engine mutex.
            // This section has no `.await`, so concurrent promotions cannot interleave.
            match promote_candidate_locked(&mut state, &payload, now_ms()) {
                Ok(success) => {
                    push_event(
                        &mut state,
                        Event::StrategyLifecycle {
                            strategy_id: success.strategy_id,
                            phase: success.phase,
                            version: success.promoted_version,
                            code_hash: success.promoted_code_hash,
                        },
                    );
                    persist_strategy_state(&state);

                    Envelope::response_to(
                        request,
                        json!({
                            "ok": true,
                            "strategy": success.summary,
                            "previous_version": success.previous_version,
                            "hard_safety_floor": "enforced",
                        }),
                    )
                }
                Err(reason) => {
                    warn!(
                        "Candidate promotion denied for {} ({}): {}",
                        payload.strategy_id, payload.code_hash, reason
                    );

                    let kill_switch_engaged = state.kill_switch_engaged;
                    push_event(
                        &mut state,
                        Event::RiskAlert {
                            level: "warning".to_string(),
                            reason: format!(
                                "promotion denied for {}:{} ({})",
                                payload.strategy_id, payload.code_hash, reason
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

            if payload.action == "reset_kill_switch" {
                // We keep it paused for safety until explicit Resume.
                apply_reset_kill_switch(&mut state);

                push_event(
                    &mut state,
                    Event::RiskAlert {
                        level: "important".to_string(),
                        reason: "manual_kill_switch_reset".to_string(),
                        kill_switch_engaged: false,
                    },
                );

                return Envelope::response_to(
                    request,
                    json!({
                        "ok": true,
                        "action": payload.action,
                        "state": risk_state_payload(&state),
                        "note": "Engine remains PAUSED after kill switch reset. Use 'resume' to start."
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

fn daemon_build_payload() -> DaemonBuildPayload {
    DaemonBuildPayload {
        name: env!("CARGO_PKG_NAME").to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        git_sha: option_env!("TRADING_DAEMON_GIT_SHA").map(str::to_string),
    }
}

fn command_kinds_supported() -> Vec<String> {
    vec![
        ControlCommand::Start.as_kind().to_string(),
        ControlCommand::Stop.as_kind().to_string(),
        ControlCommand::Status.as_kind().to_string(),
        ControlCommand::Ping.as_kind().to_string(),
        ControlCommand::Capabilities.as_kind().to_string(),
        EngineCommand::Status.as_kind().to_string(),
        EngineCommand::Pause.as_kind().to_string(),
        EngineCommand::Resume.as_kind().to_string(),
        EngineCommand::KillSwitch.as_kind().to_string(),
        StrategyCommand::List.as_kind().to_string(),
        StrategyCommand::Enable.as_kind().to_string(),
        StrategyCommand::Disable.as_kind().to_string(),
        StrategyCommand::UploadCandidate.as_kind().to_string(),
        StrategyCommand::PromoteCandidate.as_kind().to_string(),
        RiskCommand::Status.as_kind().to_string(),
        RiskCommand::Override.as_kind().to_string(),
    ]
}

fn capabilities_payload() -> CapabilitiesPayload {
    CapabilitiesPayload {
        protocol_version: PROTOCOL_VERSION,
        status_schema_version: STATUS_SCHEMA_VERSION,
        command_kinds_supported: command_kinds_supported(),
        daemon_build: daemon_build_payload(),
    }
}

fn status_response(request: &Envelope, state: &EngineState) -> Envelope {
    let recent_events: Vec<_> = state.recent_events.iter().cloned().collect();
    Envelope::response_to(
        request,
        json!({
            "ok": true,
            "protocol_version": PROTOCOL_VERSION,
            "status_schema_version": STATUS_SCHEMA_VERSION,
            "daemon_build": daemon_build_payload(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_candidate(code_hash: &str, uploaded_at_ms: i64) -> StrategyCandidate {
        StrategyCandidate {
            source: "agent".to_string(),
            code_hash: code_hash.to_string(),
            requested_canary_notional_cents: 250,
            compile_passed: true,
            replay_passed: true,
            paper_passed: true,
            latency_passed: true,
            risk_passed: true,
            uploaded_at_ms,
        }
    }

    fn promote_payload(
        strategy_id: &str,
        code_hash: &str,
        requested_canary_notional_cents: i64,
    ) -> CandidatePromotePayload {
        CandidatePromotePayload {
            strategy_id: strategy_id.to_string(),
            code_hash: code_hash.to_string(),
            requested_canary_notional_cents,
            auto: true,
        }
    }

    fn unique_state_path(label: &str) -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        format!(
            "{}/trading-daemon-test-{}-{}.json",
            std::env::temp_dir().display(),
            label,
            nanos
        )
    }

    #[test]
    fn promotion_updates_snapshot_and_preserves_enable_state() {
        let state_path = unique_state_path("promotion-preserve-enable");
        let mut state = initial_engine_state(state_path, 0);
        let strategy_id = "kalshi.arbitrage";

        {
            let strategy = state
                .strategies
                .get_mut(strategy_id)
                .expect("strategy should exist");
            strategy.enabled = false;
            strategy.candidate = Some(make_candidate("hash-1", 1_000));
            strategy.version = 3;
        }

        let payload = promote_payload(strategy_id, "hash-1", 900);
        let success = promote_candidate_locked(&mut state, &payload, 2_000)
            .expect("promotion should succeed");

        assert_eq!(success.previous_version, 3);
        assert!(!success.summary.enabled);
        assert_eq!(
            state
                .risk_snapshot
                .strategy_canary_notional
                .get(strategy_id)
                .copied(),
            Some(900)
        );
        assert!(state
            .strategies
            .get(strategy_id)
            .expect("strategy should exist")
            .candidate
            .is_none());
    }

    #[test]
    fn promotion_rejects_expired_candidate_when_ttl_enabled() {
        let state_path = unique_state_path("promotion-ttl");
        let mut state = initial_engine_state(state_path, 100);
        let strategy_id = "kalshi.market_making";

        state
            .strategies
            .get_mut(strategy_id)
            .expect("strategy should exist")
            .candidate = Some(make_candidate("hash-2", 500));

        let payload = promote_payload(strategy_id, "hash-2", 700);
        let error = promote_candidate_locked(&mut state, &payload, 700)
            .expect_err("promotion should fail due to TTL");
        assert!(error.contains("candidate expired"));
    }

    #[test]
    fn promotion_rejects_hash_mismatch() {
        let state_path = unique_state_path("promotion-hash");
        let mut state = initial_engine_state(state_path, 0);
        let strategy_id = "core.mean_reversion";

        state
            .strategies
            .get_mut(strategy_id)
            .expect("strategy should exist")
            .candidate = Some(make_candidate("expected-hash", 1_000));

        let payload = promote_payload(strategy_id, "different-hash", 600);
        let error = promote_candidate_locked(&mut state, &payload, 1_100)
            .expect_err("promotion should fail");
        assert_eq!(error, "candidate code hash mismatch");
    }

    #[test]
    fn engine_helpers_keep_pause_flags_in_sync_and_distinguish_stop_pause() {
        let state_path = unique_state_path("engine-state");
        let mut state = initial_engine_state(state_path, 0);

        apply_start(&mut state);
        assert_eq!(state.running, true);
        assert_eq!(state.paused, false);
        assert_eq!(state.risk_snapshot.paused, false);

        apply_pause(&mut state);
        assert_eq!(state.running, false);
        assert_eq!(state.paused, true);
        assert_eq!(state.risk_snapshot.paused, true);

        apply_resume(&mut state);
        assert_eq!(state.running, true);
        assert_eq!(state.paused, false);
        assert_eq!(state.risk_snapshot.paused, false);

        apply_stop(&mut state);
        assert_eq!(state.running, false);
        assert_eq!(state.paused, false);
        assert_eq!(state.risk_snapshot.paused, false);

        apply_kill_switch(&mut state);
        assert_eq!(state.running, false);
        assert_eq!(state.paused, true);
        assert_eq!(state.risk_snapshot.paused, true);
        assert_eq!(state.kill_switch_engaged, true);
        assert_eq!(state.risk_snapshot.kill_switch_engaged, true);

        apply_reset_kill_switch(&mut state);
        assert_eq!(state.running, false);
        assert_eq!(state.paused, true);
        assert_eq!(state.risk_snapshot.paused, true);
        assert_eq!(state.kill_switch_engaged, false);
        assert_eq!(state.risk_snapshot.kill_switch_engaged, false);
    }

    #[test]
    fn snapshot_roundtrip_restores_strategy_state() {
        let state_path = unique_state_path("snapshot-roundtrip");
        let mut state = initial_engine_state(state_path.clone(), 0);
        let strategy_id = "core.momentum";

        {
            let strategy = state
                .strategies
                .get_mut(strategy_id)
                .expect("strategy should exist");
            strategy.enabled = true;
            strategy.version = 7;
            strategy.canary_deployment = true;
            strategy.canary_notional_cents = 321;
            strategy.active_code_hash = Some("abc123".to_string());
            strategy.candidate = Some(make_candidate("candidate-hash", 1_234));
        }

        let snapshot = strategy_snapshot_from_state(&state);
        save_strategy_snapshot(&state_path, &snapshot).expect("snapshot should save");

        let loaded = initial_engine_state(state_path.clone(), 0);
        let loaded_strategy = loaded
            .strategies
            .get(strategy_id)
            .expect("strategy should exist");

        assert_eq!(loaded_strategy.enabled, true);
        assert_eq!(loaded_strategy.version, 7);
        assert_eq!(loaded_strategy.canary_deployment, true);
        assert_eq!(loaded_strategy.canary_notional_cents, 321);
        assert_eq!(loaded_strategy.active_code_hash.as_deref(), Some("abc123"));
        assert_eq!(
            loaded
                .risk_snapshot
                .strategy_canary_notional
                .get(strategy_id)
                .copied(),
            Some(321)
        );

        let _ = std::fs::remove_file(state_path);
    }

    #[test]
    fn capabilities_include_supported_commands_and_versions() {
        let capabilities = capabilities_payload();

        assert_eq!(capabilities.protocol_version, PROTOCOL_VERSION);
        assert_eq!(capabilities.status_schema_version, STATUS_SCHEMA_VERSION);
        assert!(capabilities
            .command_kinds_supported
            .contains(&"Control.Capabilities".to_string()));
        assert!(capabilities
            .command_kinds_supported
            .contains(&"Engine.Status".to_string()));
        assert!(capabilities
            .command_kinds_supported
            .contains(&"Risk.Status".to_string()));
    }

    #[test]
    fn status_response_includes_schema_metadata() {
        let state = initial_engine_state(unique_state_path("status-schema"), 0);
        let req = Envelope::new("Engine.Status", serde_json::json!({}));
        let resp = status_response(&req, &state);

        assert_eq!(
            resp.payload
                .get("status_schema_version")
                .and_then(|v| v.as_u64()),
            Some(u64::from(STATUS_SCHEMA_VERSION))
        );
        assert_eq!(
            resp.payload
                .get("protocol_version")
                .and_then(|v| v.as_u64()),
            Some(u64::from(PROTOCOL_VERSION))
        );
        assert_eq!(
            resp.payload
                .get("daemon_build")
                .and_then(|v| v.get("name"))
                .and_then(|v| v.as_str()),
            Some(env!("CARGO_PKG_NAME"))
        );
    }
}
