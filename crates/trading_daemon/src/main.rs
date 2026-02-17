use anyhow::{Context, Result};
use bytes::Bytes;
use exchange_coinbase_spot::CoinbaseSpotAdapter;
use exchange_core::ExchangeAdapter;
use exchange_derivatives_paper::DerivativesPaperAdapter;
use exchange_kalshi::KalshiAdapter;
use fs2::FileExt;
use futures::{SinkExt, StreamExt};
use risk_core::{HardSafetyCage, HardSafetyPolicy, PromotionRequest, RiskDecision, RiskSnapshot};
use rusqlite::{params, Connection};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, HashMap, VecDeque};
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
use trading_domain::{
    AssetClass, ExecutionMode, InstrumentId, MarketType, OptionType, OrderRequest, OrderSide,
    OrderStatus, OrderSummary, OrderType, TimeInForce,
};
use trading_protocol::{
    create_codec, CandidatePromotePayload, CandidateUploadPayload, CapabilitiesPayload,
    ControlCommand, DaemonBuildPayload, EngineCommand, EngineStatePayload, Envelope, Event,
    ExecutionModeCommand, ExecutionModePayload, InstrumentRefPayload, OrderCancelPayload,
    OrderCommand, OrderListItemPayload, OrderSubmitPayload, PortfolioBalancePayload,
    PortfolioCommand, PortfolioPositionPayload, RequestKind, RiskCommand, RiskLimitsPayload,
    RiskOverridePayload, RiskStatePayload, StrategyCommand, StrategySummaryPayload, VenueCommand,
    VenueSummaryPayload, VenueTogglePayload, DEFAULT_SOCKET_PATH, PROTOCOL_VERSION,
    STATUS_SCHEMA_VERSION,
};

const DEFAULT_LOCK_PATH: &str = "/var/run/openclaw/trading.lock";
const DEFAULT_STATE_PATH: &str = "/var/run/openclaw/trading-state.json";
const DEFAULT_DB_PATH: &str = "/var/run/openclaw/trading-state.sqlite3";
const DEFAULT_CANDIDATE_TTL_MS: i64 = 0;
const MAX_RECENT_EVENTS: usize = 128;
const SQLITE_MIGRATION_0001: &str = include_str!("../migrations/0001_runtime.sql");

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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct VenueState {
    id: String,
    enabled: bool,
    market_types: Vec<MarketType>,
    paper_only: bool,
    live_enabled: bool,
    message: Option<String>,
}

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
    db_path: String,
    candidate_ttl_ms: i64,
    venues: HashMap<String, VenueState>,
    execution_modes: HashMap<String, ExecutionMode>,
    adapters: HashMap<String, Arc<dyn ExchangeAdapter>>,
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

#[derive(Debug, Deserialize)]
struct OptionalVenuePayload {
    venue_id: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let socket_path = socket_path_from_env();
    let lock_path = lock_path_from_env();
    let state_path = state_path_from_env();
    let db_path = db_path_from_env();
    let candidate_ttl_ms = candidate_ttl_ms_from_env();
    info!("Starting trading daemon");
    info!("Socket path: {}", socket_path);
    info!("Lock path: {}", lock_path);
    info!("State path: {}", state_path);
    info!("SQLite state path: {}", db_path);
    info!("Candidate TTL (ms): {}", candidate_ttl_ms);

    ensure_socket_parent_dir(&socket_path)?;
    ensure_sqlite_path_ready(&db_path)?;
    ensure_sqlite_schema(&db_path)?;
    let _lock_file = acquire_single_instance_lock(&lock_path)?;
    let listener = bind_listener(&socket_path)?;
    let state = Arc::new(Mutex::new(initial_engine_state(
        state_path,
        db_path,
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

fn initial_engine_state(state_path: String, db_path: String, candidate_ttl_ms: i64) -> EngineState {
    let now = now_ms();
    if let Err(err) = ensure_sqlite_path_ready(&db_path) {
        warn!("Failed to prepare sqlite path {}: {:#}", db_path, err);
    }
    if let Err(err) = ensure_sqlite_schema(&db_path) {
        warn!("Failed to initialize sqlite schema {}: {:#}", db_path, err);
    }
    let venues = default_venues();
    let execution_modes = default_execution_modes(&venues);
    let adapters = default_adapters(&execution_modes);
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
        db_path,
        candidate_ttl_ms,
        venues,
        execution_modes,
        adapters,
    };

    let mut loaded_strategy_snapshot = false;
    match load_strategy_snapshot(&state.state_path) {
        Ok(Some(snapshot)) => {
            apply_strategy_snapshot(&mut state, snapshot);
            loaded_strategy_snapshot = true;
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

    match load_sqlite_runtime_state(&mut state, !loaded_strategy_snapshot) {
        Ok(()) => {
            info!("Loaded sqlite runtime state from {}", state.db_path);
        }
        Err(err) => {
            warn!(
                "Failed to load sqlite runtime state from {}: {:#}",
                state.db_path, err
            );
        }
    }

    for venue in state.venues.values() {
        if let Err(err) = sqlite_upsert_venue(&state.db_path, venue) {
            warn!(
                "Failed to persist initial venue '{}' state: {:#}",
                venue.id, err
            );
        }
    }
    for (key, mode) in &state.execution_modes {
        let mut parts = key.split(':');
        let venue_id = parts.next().unwrap_or_default();
        let market_type_raw = parts.next().unwrap_or_default();
        if let Ok(market_type) = parse_market_type(market_type_raw) {
            if let Err(err) =
                sqlite_upsert_execution_mode(&state.db_path, venue_id, market_type, *mode)
            {
                warn!(
                    "Failed to persist initial execution mode '{key}': {:#}",
                    err
                );
            }
        }
    }
    persist_sqlite_runtime_snapshot(&state);

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
        (
            "crypto.momentum_trend",
            StrategyFamily::Momentum,
            "builtin-crypto-momentum",
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

fn default_venues() -> HashMap<String, VenueState> {
    [
        VenueState {
            id: "kalshi".to_string(),
            enabled: true,
            market_types: vec![MarketType::Binary],
            paper_only: false,
            live_enabled: true,
            message: Some("US-compliant prediction venue".to_string()),
        },
        VenueState {
            id: "coinbase_spot".to_string(),
            enabled: true,
            market_types: vec![MarketType::Spot],
            paper_only: false,
            live_enabled: true,
            message: Some("US-compliant crypto spot venue".to_string()),
        },
        VenueState {
            id: "derivatives_paper".to_string(),
            enabled: true,
            market_types: vec![
                MarketType::Perpetual,
                MarketType::Futures,
                MarketType::Option,
            ],
            paper_only: true,
            live_enabled: false,
            message: Some("paper-only derivatives fallback".to_string()),
        },
    ]
    .into_iter()
    .map(|v| (v.id.clone(), v))
    .collect()
}

fn mode_key(venue_id: &str, market_type: MarketType) -> String {
    format!("{venue_id}:{market_type:?}")
}

fn default_execution_modes(venues: &HashMap<String, VenueState>) -> HashMap<String, ExecutionMode> {
    let mut modes = HashMap::new();
    for venue in venues.values() {
        for market_type in &venue.market_types {
            let mode = if venue.paper_only {
                ExecutionMode::Paper
            } else {
                ExecutionMode::Live
            };
            modes.insert(mode_key(&venue.id, *market_type), mode);
        }
    }
    modes
}

fn default_adapters(
    execution_modes: &HashMap<String, ExecutionMode>,
) -> HashMap<String, Arc<dyn ExchangeAdapter>> {
    let kalshi_mode = execution_modes
        .get(&mode_key("kalshi", MarketType::Binary))
        .copied()
        .unwrap_or(ExecutionMode::Paper);
    let coinbase_mode = execution_modes
        .get(&mode_key("coinbase_spot", MarketType::Spot))
        .copied()
        .unwrap_or(ExecutionMode::Paper);

    [
        (
            "kalshi".to_string(),
            Arc::new(KalshiAdapter::new(kalshi_mode)) as Arc<dyn ExchangeAdapter>,
        ),
        (
            "coinbase_spot".to_string(),
            Arc::new(CoinbaseSpotAdapter::new(coinbase_mode)) as Arc<dyn ExchangeAdapter>,
        ),
        (
            "derivatives_paper".to_string(),
            Arc::new(DerivativesPaperAdapter::default()) as Arc<dyn ExchangeAdapter>,
        ),
    ]
    .into_iter()
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

fn db_path_from_env() -> String {
    std::env::var("TRADING_DB_PATH").unwrap_or_else(|_| DEFAULT_DB_PATH.to_string())
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

fn ensure_sqlite_path_ready(db_path: &str) -> Result<()> {
    if let Some(parent) = Path::new(db_path).parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "Failed to create sqlite directory for database {}",
                parent.display()
            )
        })?;
    }
    Ok(())
}

fn ensure_sqlite_schema(db_path: &str) -> Result<()> {
    let conn = Connection::open(db_path)
        .with_context(|| format!("Failed to open sqlite database at {}", db_path))?;
    conn.execute_batch(SQLITE_MIGRATION_0001)
        .with_context(|| format!("Failed to initialize sqlite schema at {}", db_path))?;
    Ok(())
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
        Some(RequestKind::Venue(command)) => process_venue_request(request, state, command).await,
        Some(RequestKind::Portfolio(command)) => {
            process_portfolio_request(request, state, command).await
        }
        Some(RequestKind::Order(command)) => process_order_request(request, state, command).await,
        Some(RequestKind::ExecutionMode(command)) => {
            process_execution_mode_request(request, state, command).await
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
            persist_sqlite_runtime_snapshot(&state);
            status_response(request, &state)
        }
        ControlCommand::Stop => {
            let mut state = state.lock().await;
            apply_stop(&mut state);
            let event = engine_health_event(&state);
            push_event(&mut state, event);
            persist_sqlite_runtime_snapshot(&state);
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
            persist_sqlite_runtime_snapshot(&state);
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
            persist_sqlite_runtime_snapshot(&state);
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
            persist_sqlite_runtime_snapshot(&state);
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
            persist_sqlite_runtime_snapshot(&state);

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
            persist_sqlite_runtime_snapshot(&state);

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
                    persist_sqlite_runtime_snapshot(&state);

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
                    persist_sqlite_runtime_snapshot(&state);

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
                persist_sqlite_runtime_snapshot(&state);
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
                persist_sqlite_runtime_snapshot(&state);

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

fn bool_to_i64(value: bool) -> i64 {
    if value {
        1
    } else {
        0
    }
}

fn i64_to_bool(value: i64) -> bool {
    value != 0
}

fn asset_class_label(value: AssetClass) -> String {
    match value {
        AssetClass::Prediction => "prediction",
        AssetClass::Crypto => "crypto",
        AssetClass::Equity => "equity",
        AssetClass::Forex => "forex",
        AssetClass::Derivative => "derivative",
        AssetClass::Unknown => "unknown",
    }
    .to_string()
}

fn market_type_label(value: MarketType) -> String {
    match value {
        MarketType::Spot => "spot",
        MarketType::Binary => "binary",
        MarketType::Perpetual => "perpetual",
        MarketType::Futures => "futures",
        MarketType::Option => "option",
        MarketType::Unknown => "unknown",
    }
    .to_string()
}

fn order_side_label(value: OrderSide) -> String {
    match value {
        OrderSide::Buy => "buy",
        OrderSide::Sell => "sell",
    }
    .to_string()
}

fn order_type_label(value: OrderType) -> String {
    match value {
        OrderType::Limit => "limit",
        OrderType::Market => "market",
    }
    .to_string()
}

fn order_status_label(value: OrderStatus) -> String {
    match value {
        OrderStatus::Pending => "pending",
        OrderStatus::Accepted => "accepted",
        OrderStatus::Filled => "filled",
        OrderStatus::Canceled => "canceled",
        OrderStatus::Rejected => "rejected",
        OrderStatus::Failed => "failed",
    }
    .to_string()
}

fn execution_mode_label(value: ExecutionMode) -> String {
    match value {
        ExecutionMode::Paper => "paper",
        ExecutionMode::Live => "live",
    }
    .to_string()
}

fn option_type_label(value: OptionType) -> String {
    match value {
        OptionType::Call => "call",
        OptionType::Put => "put",
    }
    .to_string()
}

fn parse_asset_class(raw: &str) -> std::result::Result<AssetClass, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "prediction" => Ok(AssetClass::Prediction),
        "crypto" => Ok(AssetClass::Crypto),
        "equity" => Ok(AssetClass::Equity),
        "forex" => Ok(AssetClass::Forex),
        "derivative" | "derivatives" => Ok(AssetClass::Derivative),
        "unknown" => Ok(AssetClass::Unknown),
        other => Err(format!("unsupported asset_class '{other}'")),
    }
}

fn parse_market_type(raw: &str) -> std::result::Result<MarketType, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "spot" => Ok(MarketType::Spot),
        "binary" => Ok(MarketType::Binary),
        "perpetual" | "perp" => Ok(MarketType::Perpetual),
        "futures" | "future" => Ok(MarketType::Futures),
        "option" | "options" => Ok(MarketType::Option),
        "unknown" => Ok(MarketType::Unknown),
        other => Err(format!("unsupported market_type '{other}'")),
    }
}

fn parse_order_side(raw: &str) -> std::result::Result<OrderSide, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "buy" => Ok(OrderSide::Buy),
        "sell" => Ok(OrderSide::Sell),
        other => Err(format!("unsupported side '{other}'")),
    }
}

fn parse_order_type(raw: &str) -> std::result::Result<OrderType, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "limit" => Ok(OrderType::Limit),
        "market" => Ok(OrderType::Market),
        other => Err(format!("unsupported order_type '{other}'")),
    }
}

fn parse_tif(raw: &str) -> std::result::Result<TimeInForce, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "gtc" => Ok(TimeInForce::Gtc),
        "ioc" => Ok(TimeInForce::Ioc),
        "fok" => Ok(TimeInForce::Fok),
        "day" => Ok(TimeInForce::Day),
        other => Err(format!("unsupported tif '{other}'")),
    }
}

fn parse_execution_mode(raw: &str) -> std::result::Result<ExecutionMode, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "paper" => Ok(ExecutionMode::Paper),
        "live" => Ok(ExecutionMode::Live),
        other => Err(format!("unsupported mode '{other}'")),
    }
}

fn parse_option_type(raw: &str) -> std::result::Result<OptionType, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "call" => Ok(OptionType::Call),
        "put" => Ok(OptionType::Put),
        other => Err(format!("unsupported option_type '{other}'")),
    }
}

fn instrument_to_payload(instrument: &InstrumentId) -> InstrumentRefPayload {
    InstrumentRefPayload {
        venue_id: instrument.venue_id.clone(),
        symbol: instrument.symbol.clone(),
        asset_class: asset_class_label(instrument.asset_class),
        market_type: market_type_label(instrument.market_type),
        expiry_ts_ms: instrument.expiry_ts_ms,
        strike: instrument.strike,
        option_type: instrument.option_type.map(option_type_label),
    }
}

fn payload_to_instrument(
    payload: &InstrumentRefPayload,
) -> std::result::Result<InstrumentId, String> {
    let mut metadata = BTreeMap::new();
    metadata.insert("source".to_string(), "protocol".to_string());

    Ok(InstrumentId {
        venue_id: payload.venue_id.clone(),
        symbol: payload.symbol.clone(),
        asset_class: parse_asset_class(&payload.asset_class)?,
        market_type: parse_market_type(&payload.market_type)?,
        expiry_ts_ms: payload.expiry_ts_ms,
        strike: payload.strike,
        option_type: payload
            .option_type
            .as_ref()
            .map(|v| parse_option_type(v))
            .transpose()?,
        metadata,
    })
}

fn venue_summary_from_state(state: &EngineState, venue: &VenueState) -> VenueSummaryPayload {
    let health = state
        .adapters
        .get(&venue.id)
        .map(|adapter| adapter.health())
        .unwrap_or_else(|| trading_domain::VenueHealth {
            venue_id: venue.id.clone(),
            healthy: false,
            connected_market_data: false,
            connected_trading: false,
            message: Some("adapter unavailable".to_string()),
        });

    VenueSummaryPayload {
        venue_id: venue.id.clone(),
        enabled: venue.enabled,
        market_types: venue
            .market_types
            .iter()
            .map(|m| market_type_label(*m))
            .collect(),
        healthy: health.healthy,
        live_enabled: venue.live_enabled,
        paper_only: venue.paper_only,
        message: venue.message.clone().or(health.message),
    }
}

fn order_summary_to_payload(order: &OrderSummary) -> OrderListItemPayload {
    OrderListItemPayload {
        venue_order_id: order.venue_order_id.clone(),
        client_order_id: order.client_order_id.clone(),
        strategy_id: order.strategy_id.clone(),
        venue_id: order.venue_id.clone(),
        instrument: instrument_to_payload(&order.instrument),
        side: order_side_label(order.side),
        order_type: order_type_label(order.order_type),
        quantity: order.quantity,
        filled_quantity: order.filled_quantity,
        avg_fill_price: order.avg_fill_price,
        status: order_status_label(order.status),
        created_at_ms: order.created_at_ms,
        updated_at_ms: order.updated_at_ms,
        message: order.message.clone(),
    }
}

fn refresh_adapter_for_venue(state: &mut EngineState, venue_id: &str) {
    match venue_id {
        "kalshi" => {
            let mode = state
                .execution_modes
                .get(&mode_key("kalshi", MarketType::Binary))
                .copied()
                .unwrap_or(ExecutionMode::Paper);
            state
                .adapters
                .insert("kalshi".to_string(), Arc::new(KalshiAdapter::new(mode)));
        }
        "coinbase_spot" => {
            let mode = state
                .execution_modes
                .get(&mode_key("coinbase_spot", MarketType::Spot))
                .copied()
                .unwrap_or(ExecutionMode::Paper);
            state.adapters.insert(
                "coinbase_spot".to_string(),
                Arc::new(CoinbaseSpotAdapter::new(mode)),
            );
        }
        "derivatives_paper" => {
            state.adapters.insert(
                "derivatives_paper".to_string(),
                Arc::new(DerivativesPaperAdapter::default()),
            );
        }
        _ => {}
    }
}

fn sqlite_upsert_venue(db_path: &str, venue: &VenueState) -> Result<()> {
    let conn = Connection::open(db_path)
        .with_context(|| format!("failed opening sqlite database {}", db_path))?;
    let market_types = serde_json::to_string(&venue.market_types).context("encode market types")?;
    conn.execute(
        r#"
        INSERT INTO venues (venue_id, enabled, market_types, paper_only, live_enabled, message, updated_at_ms)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
        ON CONFLICT(venue_id) DO UPDATE SET
            enabled=excluded.enabled,
            market_types=excluded.market_types,
            paper_only=excluded.paper_only,
            live_enabled=excluded.live_enabled,
            message=excluded.message,
            updated_at_ms=excluded.updated_at_ms
        "#,
        params![
            venue.id,
            bool_to_i64(venue.enabled),
            market_types,
            bool_to_i64(venue.paper_only),
            bool_to_i64(venue.live_enabled),
            venue.message,
            now_ms(),
        ],
    )
    .context("upsert venue failed")?;
    Ok(())
}

fn sqlite_upsert_execution_mode(
    db_path: &str,
    venue_id: &str,
    market_type: MarketType,
    mode: ExecutionMode,
) -> Result<()> {
    let conn = Connection::open(db_path)
        .with_context(|| format!("failed opening sqlite database {}", db_path))?;
    let key = mode_key(venue_id, market_type);
    conn.execute(
        r#"
        INSERT INTO execution_modes (mode_key, venue_id, market_type, mode, updated_at_ms)
        VALUES (?1, ?2, ?3, ?4, ?5)
        ON CONFLICT(mode_key) DO UPDATE SET
            mode=excluded.mode,
            updated_at_ms=excluded.updated_at_ms
        "#,
        params![
            key,
            venue_id,
            market_type_label(market_type),
            execution_mode_label(mode),
            now_ms(),
        ],
    )
    .context("upsert execution mode failed")?;
    Ok(())
}

fn sqlite_upsert_order(db_path: &str, order: &OrderSummary) -> Result<()> {
    let conn = Connection::open(db_path)
        .with_context(|| format!("failed opening sqlite database {}", db_path))?;
    let payload = serde_json::to_string(order).context("encode order summary")?;
    conn.execute(
        r#"
        INSERT INTO orders (venue_order_id, payload_json, updated_at_ms)
        VALUES (?1, ?2, ?3)
        ON CONFLICT(venue_order_id) DO UPDATE SET
            payload_json=excluded.payload_json,
            updated_at_ms=excluded.updated_at_ms
        "#,
        params![order.venue_order_id, payload, now_ms()],
    )
    .context("upsert order failed")?;
    Ok(())
}

fn sqlite_upsert_fill(db_path: &str, fill_id: &str, payload: &serde_json::Value) -> Result<()> {
    let conn = Connection::open(db_path)
        .with_context(|| format!("failed opening sqlite database {}", db_path))?;
    conn.execute(
        r#"
        INSERT INTO fills (fill_id, payload_json, updated_at_ms)
        VALUES (?1, ?2, ?3)
        ON CONFLICT(fill_id) DO UPDATE SET
            payload_json=excluded.payload_json,
            updated_at_ms=excluded.updated_at_ms
        "#,
        params![fill_id, serde_json::to_string(payload)?, now_ms()],
    )
    .context("upsert fill failed")?;
    Ok(())
}

fn sqlite_upsert_strategy(db_path: &str, strategy: &StrategyState) -> Result<()> {
    let conn = Connection::open(db_path)
        .with_context(|| format!("failed opening sqlite database {}", db_path))?;
    conn.execute(
        r#"
        INSERT INTO strategies (strategy_id, payload_json, updated_at_ms)
        VALUES (?1, ?2, ?3)
        ON CONFLICT(strategy_id) DO UPDATE SET
            payload_json=excluded.payload_json,
            updated_at_ms=excluded.updated_at_ms
        "#,
        params![&strategy.id, serde_json::to_string(strategy)?, now_ms()],
    )
    .context("upsert strategy failed")?;
    Ok(())
}

fn sqlite_upsert_risk_state(db_path: &str, snapshot: &RiskSnapshot) -> Result<()> {
    let conn = Connection::open(db_path)
        .with_context(|| format!("failed opening sqlite database {}", db_path))?;
    conn.execute(
        r#"
        INSERT INTO risk_state (id, payload_json, updated_at_ms)
        VALUES (1, ?1, ?2)
        ON CONFLICT(id) DO UPDATE SET
            payload_json=excluded.payload_json,
            updated_at_ms=excluded.updated_at_ms
        "#,
        params![serde_json::to_string(snapshot)?, now_ms()],
    )
    .context("upsert risk state failed")?;
    Ok(())
}

fn sqlite_append_event(db_path: &str, value: &serde_json::Value) -> Result<()> {
    let conn = Connection::open(db_path)
        .with_context(|| format!("failed opening sqlite database {}", db_path))?;
    conn.execute(
        "INSERT INTO events_journal (ts_ms, event_json) VALUES (?1, ?2)",
        params![now_ms(), serde_json::to_string(value)?],
    )
    .context("append event failed")?;
    Ok(())
}

fn persist_sqlite_runtime_snapshot(state: &EngineState) {
    for strategy in state.strategies.values() {
        if let Err(err) = sqlite_upsert_strategy(&state.db_path, strategy) {
            warn!(
                "Failed to persist strategy '{}' runtime state: {:#}",
                strategy.id, err
            );
        }
    }
    if let Err(err) = sqlite_upsert_risk_state(&state.db_path, &state.risk_snapshot) {
        warn!("Failed to persist risk runtime state: {:#}", err);
    }
}

fn load_sqlite_runtime_state(
    state: &mut EngineState,
    restore_strategy_and_risk: bool,
) -> Result<()> {
    let conn = Connection::open(&state.db_path)
        .with_context(|| format!("failed opening sqlite database {}", state.db_path))?;

    {
        let mut stmt = conn.prepare(
            "SELECT venue_id, enabled, market_types, paper_only, live_enabled, message FROM venues",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let venue_id: String = row.get(0)?;
            let enabled: i64 = row.get(1)?;
            let market_types_json: String = row.get(2)?;
            let paper_only: i64 = row.get(3)?;
            let live_enabled: i64 = row.get(4)?;
            let message: Option<String> = row.get(5)?;

            let market_types: Vec<MarketType> =
                serde_json::from_str(&market_types_json).unwrap_or_default();
            if market_types.is_empty() {
                continue;
            }

            if let Some(venue) = state.venues.get_mut(&venue_id) {
                venue.enabled = i64_to_bool(enabled);
                venue.paper_only = i64_to_bool(paper_only);
                venue.live_enabled = i64_to_bool(live_enabled);
                venue.message = message;
                venue.market_types = market_types;
            }
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT venue_id, market_type, mode FROM execution_modes ORDER BY mode_key")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let venue_id: String = row.get(0)?;
            let market_type_raw: String = row.get(1)?;
            let mode_raw: String = row.get(2)?;
            let Ok(market_type) = parse_market_type(&market_type_raw) else {
                continue;
            };
            let Ok(mode) = parse_execution_mode(&mode_raw) else {
                continue;
            };
            state
                .execution_modes
                .insert(mode_key(&venue_id, market_type), mode);
        }
    }

    if restore_strategy_and_risk {
        {
            let mut loaded: Vec<StrategyState> = Vec::new();
            let mut stmt =
                conn.prepare("SELECT payload_json FROM strategies ORDER BY strategy_id")?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let payload_json: String = row.get(0)?;
                match serde_json::from_str::<StrategyState>(&payload_json) {
                    Ok(strategy) => loaded.push(strategy),
                    Err(err) => warn!("Ignoring invalid sqlite strategy payload: {}", err),
                }
            }
            if !loaded.is_empty() {
                apply_strategy_snapshot(
                    state,
                    StrategyStateSnapshot {
                        schema_version: 1,
                        saved_at_ms: now_ms(),
                        strategies: loaded,
                    },
                );
            }
        }

        {
            let mut stmt = conn.prepare("SELECT payload_json FROM risk_state WHERE id = 1")?;
            let mut rows = stmt.query([])?;
            if let Some(row) = rows.next()? {
                let payload_json: String = row.get(0)?;
                match serde_json::from_str::<RiskSnapshot>(&payload_json) {
                    Ok(snapshot) => {
                        state.risk_snapshot = snapshot;
                    }
                    Err(err) => warn!("Ignoring invalid sqlite risk state payload: {}", err),
                }
            }
        }
    }

    state.risk_snapshot.strategy_canary_notional = strategy_canary_notional_map(&state.strategies);

    state.adapters = default_adapters(&state.execution_modes);
    Ok(())
}

async fn process_venue_request(
    request: &Envelope,
    state: &Arc<Mutex<EngineState>>,
    command: VenueCommand,
) -> Envelope {
    match command {
        VenueCommand::List => {
            let state = state.lock().await;
            let mut venues: Vec<_> = state
                .venues
                .values()
                .map(|v| venue_summary_from_state(&state, v))
                .collect();
            venues.sort_by(|a, b| a.venue_id.cmp(&b.venue_id));
            Envelope::response_to(request, json!({ "ok": true, "venues": venues }))
        }
        VenueCommand::Status | VenueCommand::Enable | VenueCommand::Disable => {
            let payload: VenueTogglePayload = match parse_payload(&request.payload) {
                Ok(p) => p,
                Err(err) => {
                    return Envelope::response_to(request, json!({"ok": false, "error": err}));
                }
            };

            let mut state = state.lock().await;
            if matches!(command, VenueCommand::Enable | VenueCommand::Disable) {
                let enabled = matches!(command, VenueCommand::Enable);
                let db_path = state.db_path.clone();
                let venue_to_persist = match state.venues.get_mut(&payload.venue_id) {
                    Some(v) => v,
                    None => {
                        return Envelope::response_to(
                            request,
                            json!({ "ok": false, "error": format!("Unknown venue '{}'", payload.venue_id) }),
                        );
                    }
                };
                venue_to_persist.enabled = enabled;
                let venue_to_persist = venue_to_persist.clone();
                state.last_command_at_ms = now_ms();
                if let Err(err) = sqlite_upsert_venue(&db_path, &venue_to_persist) {
                    warn!("Failed to persist venue state: {:#}", err);
                }
            }

            let Some(venue) = state.venues.get(&payload.venue_id).cloned() else {
                return Envelope::response_to(
                    request,
                    json!({ "ok": false, "error": format!("Unknown venue '{}'", payload.venue_id) }),
                );
            };
            let summary = venue_summary_from_state(&state, &venue);
            Envelope::response_to(request, json!({ "ok": true, "venue": summary }))
        }
    }
}

async fn process_portfolio_request(
    request: &Envelope,
    state: &Arc<Mutex<EngineState>>,
    command: PortfolioCommand,
) -> Envelope {
    let adapters: Vec<Arc<dyn ExchangeAdapter>> = {
        let state = state.lock().await;
        state
            .venues
            .values()
            .filter(|venue| venue.enabled)
            .filter_map(|venue| state.adapters.get(&venue.id).cloned())
            .collect()
    };

    let mut balances: Vec<PortfolioBalancePayload> = Vec::new();
    let mut positions: Vec<PortfolioPositionPayload> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    for adapter in adapters {
        match command {
            PortfolioCommand::Balances => match adapter.sync_balances().await {
                Ok(values) => {
                    balances.extend(values.into_iter().map(|value| PortfolioBalancePayload {
                        venue_id: value.venue_id,
                        asset: value.asset,
                        total: value.total,
                        available: value.available,
                    }));
                }
                Err(err) => errors.push(format!("{}: {}", adapter.venue(), err.message)),
            },
            PortfolioCommand::Positions => match adapter.sync_positions().await {
                Ok(values) => {
                    positions.extend(values.into_iter().map(|value| PortfolioPositionPayload {
                        venue_id: value.venue_id,
                        instrument: instrument_to_payload(&value.instrument),
                        quantity: value.quantity,
                        avg_price: value.avg_price,
                        mark_price: value.mark_price,
                        unrealized_pnl: value.unrealized_pnl,
                    }));
                }
                Err(err) => errors.push(format!("{}: {}", adapter.venue(), err.message)),
            },
        }
    }

    match command {
        PortfolioCommand::Balances => Envelope::response_to(
            request,
            json!({ "ok": errors.is_empty(), "balances": balances, "errors": errors }),
        ),
        PortfolioCommand::Positions => Envelope::response_to(
            request,
            json!({ "ok": errors.is_empty(), "positions": positions, "errors": errors }),
        ),
    }
}

async fn process_order_request(
    request: &Envelope,
    state: &Arc<Mutex<EngineState>>,
    command: OrderCommand,
) -> Envelope {
    match command {
        OrderCommand::Submit => {
            let payload: OrderSubmitPayload = match parse_payload(&request.payload) {
                Ok(p) => p,
                Err(err) => {
                    return Envelope::response_to(request, json!({"ok": false, "error": err}));
                }
            };

            let instrument = match payload_to_instrument(&payload.instrument) {
                Ok(v) => v,
                Err(err) => {
                    return Envelope::response_to(request, json!({"ok": false, "error": err}));
                }
            };
            let side = match parse_order_side(&payload.side) {
                Ok(v) => v,
                Err(err) => {
                    return Envelope::response_to(request, json!({"ok": false, "error": err}));
                }
            };
            let order_type = match parse_order_type(&payload.order_type) {
                Ok(v) => v,
                Err(err) => {
                    return Envelope::response_to(request, json!({"ok": false, "error": err}));
                }
            };
            let tif = match payload.tif {
                Some(raw) => match parse_tif(&raw) {
                    Ok(v) => Some(v),
                    Err(err) => {
                        return Envelope::response_to(request, json!({"ok": false, "error": err}));
                    }
                },
                None => None,
            };

            let requested_notional_cents = {
                let base = if let Some(limit_price) = payload.limit_price {
                    (payload.quantity * limit_price * 100.0).round() as i64
                } else {
                    (payload.quantity * 100.0).round() as i64
                };
                base.max(1)
            };

            let (adapter, venue_id, db_path, can_submit) = {
                let state = state.lock().await;
                let Some(strategy) = state.strategies.get(&payload.strategy_id) else {
                    return Envelope::response_to(
                        request,
                        json!({"ok": false, "error": format!("Unknown strategy '{}'", payload.strategy_id)}),
                    );
                };
                if !strategy.enabled {
                    return Envelope::response_to(
                        request,
                        json!({"ok": false, "error": format!("Strategy '{}' is disabled", payload.strategy_id)}),
                    );
                }
                let Some(venue) = state.venues.get(&payload.venue_id) else {
                    return Envelope::response_to(
                        request,
                        json!({"ok": false, "error": format!("Unknown venue '{}'", payload.venue_id)}),
                    );
                };
                if !venue.enabled {
                    return Envelope::response_to(
                        request,
                        json!({"ok": false, "error": format!("Venue '{}' is disabled", payload.venue_id)}),
                    );
                }
                let market_type = instrument.market_type;
                let mode = state
                    .execution_modes
                    .get(&mode_key(&payload.venue_id, market_type))
                    .copied()
                    .unwrap_or(ExecutionMode::Paper);
                let can_submit = if mode == ExecutionMode::Live {
                    venue.live_enabled && !venue.paper_only
                } else {
                    true
                };
                let cage = HardSafetyCage::new(state.safety_policy.clone());
                let risk_decision = cage.evaluate_order_with_context(
                    &payload.strategy_id,
                    Some(&payload.venue_id),
                    requested_notional_cents,
                    &state.risk_snapshot,
                );
                if let RiskDecision::Deny { reason } = risk_decision {
                    return Envelope::response_to(
                        request,
                        json!({
                            "ok": false,
                            "error": reason,
                            "hard_safety_floor": "enforced",
                        }),
                    );
                }
                (
                    state.adapters.get(&payload.venue_id).cloned(),
                    payload.venue_id.clone(),
                    state.db_path.clone(),
                    can_submit,
                )
            };

            if !can_submit {
                return Envelope::response_to(
                    request,
                    json!({"ok": false, "error": "venue is not authorized for live execution"}),
                );
            }

            let Some(adapter) = adapter else {
                return Envelope::response_to(
                    request,
                    json!({"ok": false, "error": format!("No adapter for venue '{}'", venue_id)}),
                );
            };

            let req = OrderRequest {
                venue_id: payload.venue_id.clone(),
                strategy_id: payload.strategy_id.clone(),
                client_order_id: payload.client_order_id.clone(),
                instrument,
                side,
                order_type,
                quantity: payload.quantity,
                limit_price: payload.limit_price,
                tif,
                post_only: payload.post_only,
                reduce_only: payload.reduce_only,
                metadata: BTreeMap::new(),
            };

            match adapter.place_order(req.clone()).await {
                Ok(ack) => {
                    let now = now_ms();
                    let summary = OrderSummary {
                        venue_order_id: ack.venue_order_id.clone(),
                        client_order_id: req.client_order_id.clone(),
                        strategy_id: req.strategy_id.clone(),
                        venue_id: req.venue_id.clone(),
                        instrument: req.instrument.clone(),
                        side: req.side,
                        order_type: req.order_type,
                        quantity: req.quantity,
                        filled_quantity: if ack.status == OrderStatus::Filled {
                            req.quantity
                        } else {
                            0.0
                        },
                        avg_fill_price: req.limit_price,
                        status: ack.status,
                        created_at_ms: now,
                        updated_at_ms: now,
                        message: ack.reason.clone(),
                    };
                    if let Err(err) = sqlite_upsert_order(&db_path, &summary) {
                        warn!("Failed to persist order summary: {:#}", err);
                    }
                    if ack.status == OrderStatus::Filled {
                        let fill_id = format!("{}:{}", ack.venue_order_id, now);
                        let fill = json!({
                            "fill_id": fill_id,
                            "venue_order_id": ack.venue_order_id,
                            "client_order_id": req.client_order_id.clone(),
                            "strategy_id": req.strategy_id.clone(),
                            "venue_id": req.venue_id.clone(),
                            "symbol": req.instrument.symbol.clone(),
                            "quantity": req.quantity,
                            "price": req.limit_price,
                            "ts_ms": now,
                        });
                        if let Err(err) = sqlite_upsert_fill(&db_path, &fill_id, &fill) {
                            warn!("Failed to persist fill: {:#}", err);
                        }
                    }

                    let mut state = state.lock().await;
                    state.last_command_at_ms = now;
                    state.risk_snapshot.orders_last_minute =
                        state.risk_snapshot.orders_last_minute.saturating_add(1);
                    state.risk_snapshot.total_notional_cents = state
                        .risk_snapshot
                        .total_notional_cents
                        .saturating_add(requested_notional_cents);
                    let venue_total = state
                        .risk_snapshot
                        .venue_notional_cents
                        .entry(req.venue_id.clone())
                        .or_insert(0);
                    *venue_total = venue_total.saturating_add(requested_notional_cents);
                    let strategy_total = state
                        .risk_snapshot
                        .strategy_canary_notional
                        .entry(req.strategy_id.clone())
                        .or_insert(0);
                    *strategy_total = strategy_total.saturating_add(requested_notional_cents);
                    if ack.status == OrderStatus::Filled {
                        state.risk_snapshot.open_positions =
                            state.risk_snapshot.open_positions.saturating_add(1);
                    }
                    push_event(
                        &mut state,
                        Event::Execution {
                            venue: req.venue_id,
                            strategy_id: req.strategy_id,
                            symbol: req.instrument.symbol,
                            action: order_side_label(req.side),
                            status: order_status_label(ack.status),
                            latency_ms: 0,
                        },
                    );
                    persist_sqlite_runtime_snapshot(&state);

                    Envelope::response_to(
                        request,
                        json!({
                            "ok": true,
                            "ack": {
                                "venue_order_id": ack.venue_order_id,
                                "accepted": ack.accepted,
                                "status": order_status_label(ack.status),
                                "reason": ack.reason,
                            }
                        }),
                    )
                }
                Err(err) => Envelope::response_to(
                    request,
                    json!({ "ok": false, "error": err.message, "code": err.code }),
                ),
            }
        }
        OrderCommand::Cancel => {
            let payload: OrderCancelPayload = match parse_payload(&request.payload) {
                Ok(p) => p,
                Err(err) => {
                    return Envelope::response_to(request, json!({"ok": false, "error": err}));
                }
            };

            let adapters: Vec<Arc<dyn ExchangeAdapter>> = {
                let state = state.lock().await;
                if let Some(venue_id) = payload.venue_id.as_ref() {
                    state
                        .adapters
                        .get(venue_id)
                        .cloned()
                        .map(|a| vec![a])
                        .unwrap_or_default()
                } else {
                    state.adapters.values().cloned().collect()
                }
            };

            for adapter in adapters {
                if adapter.cancel_order(&payload.venue_order_id).await.is_ok() {
                    return Envelope::response_to(
                        request,
                        json!({ "ok": true, "venue_order_id": payload.venue_order_id }),
                    );
                }
            }

            Envelope::response_to(
                request,
                json!({ "ok": false, "error": "order not found in enabled venues" }),
            )
        }
        OrderCommand::List => {
            let payload: OptionalVenuePayload =
                parse_payload(&request.payload).unwrap_or(OptionalVenuePayload { venue_id: None });

            let adapters: Vec<Arc<dyn ExchangeAdapter>> = {
                let state = state.lock().await;
                match payload.venue_id.as_ref() {
                    Some(venue_id) => state
                        .adapters
                        .get(venue_id)
                        .cloned()
                        .map(|a| vec![a])
                        .unwrap_or_default(),
                    None => state.adapters.values().cloned().collect(),
                }
            };

            let mut orders: Vec<OrderListItemPayload> = Vec::new();
            for adapter in adapters {
                if let Ok(adapter_orders) = adapter.list_orders().await {
                    orders.extend(
                        adapter_orders
                            .into_iter()
                            .map(|o| order_summary_to_payload(&o)),
                    );
                }
            }
            orders.sort_by(|a, b| b.updated_at_ms.cmp(&a.updated_at_ms));

            Envelope::response_to(request, json!({ "ok": true, "orders": orders }))
        }
    }
}

async fn process_execution_mode_request(
    request: &Envelope,
    state: &Arc<Mutex<EngineState>>,
    command: ExecutionModeCommand,
) -> Envelope {
    let payload: ExecutionModePayload = match parse_payload(&request.payload) {
        Ok(p) => p,
        Err(err) => {
            return Envelope::response_to(request, json!({"ok": false, "error": err}));
        }
    };

    let market_type = match parse_market_type(&payload.market_type) {
        Ok(v) => v,
        Err(err) => {
            return Envelope::response_to(request, json!({"ok": false, "error": err}));
        }
    };

    let mut state = state.lock().await;
    let venue = match state.venues.get(&payload.venue_id) {
        Some(v) => v.clone(),
        None => {
            return Envelope::response_to(
                request,
                json!({"ok": false, "error": format!("Unknown venue '{}'", payload.venue_id)}),
            );
        }
    };

    if !venue.market_types.contains(&market_type) {
        return Envelope::response_to(
            request,
            json!({"ok": false, "error": "venue does not support requested market_type"}),
        );
    }

    let key = mode_key(&payload.venue_id, market_type);
    match command {
        ExecutionModeCommand::Get => {
            let mode = state
                .execution_modes
                .get(&key)
                .copied()
                .unwrap_or(ExecutionMode::Paper);
            Envelope::response_to(
                request,
                json!({
                    "ok": true,
                    "execution_mode": {
                        "venue_id": payload.venue_id,
                        "market_type": market_type_label(market_type),
                        "mode": execution_mode_label(mode),
                    }
                }),
            )
        }
        ExecutionModeCommand::Set => {
            let mode = match parse_execution_mode(&payload.mode) {
                Ok(v) => v,
                Err(err) => {
                    return Envelope::response_to(request, json!({"ok": false, "error": err}));
                }
            };

            if mode == ExecutionMode::Live && (venue.paper_only || !venue.live_enabled) {
                return Envelope::response_to(
                    request,
                    json!({"ok": false, "error": "venue cannot run in live mode"}),
                );
            }

            state.execution_modes.insert(key, mode);
            refresh_adapter_for_venue(&mut state, &payload.venue_id);
            if let Err(err) =
                sqlite_upsert_execution_mode(&state.db_path, &payload.venue_id, market_type, mode)
            {
                warn!("Failed to persist execution mode: {:#}", err);
            }

            Envelope::response_to(
                request,
                json!({
                    "ok": true,
                    "execution_mode": {
                        "venue_id": payload.venue_id,
                        "market_type": market_type_label(market_type),
                        "mode": execution_mode_label(mode),
                    }
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
    let mut venues: Vec<_> = state
        .venues
        .values()
        .map(|venue| venue_summary_from_state(state, venue))
        .collect();
    venues.sort_by(|a, b| a.venue_id.cmp(&b.venue_id));

    let mut execution_modes: Vec<_> = state
        .execution_modes
        .iter()
        .filter_map(|(key, mode)| {
            let mut parts = key.split(':');
            let venue_id = parts.next()?.to_string();
            let market_type = parts.next()?.to_ascii_lowercase();
            Some(json!({
                "venue_id": venue_id,
                "market_type": market_type,
                "mode": execution_mode_label(*mode),
            }))
        })
        .collect();
    execution_modes.sort_by(|a, b| {
        let a_venue = a
            .get("venue_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let b_venue = b
            .get("venue_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        a_venue.cmp(b_venue)
    });

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
            "venues": venues,
            "execution_modes": execution_modes,
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

    if let Some(last) = state.recent_events.back() {
        if let Err(err) = sqlite_append_event(&state.db_path, last) {
            warn!("Failed to persist event journal record: {:#}", err);
        }
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

    fn unique_db_path(label: &str) -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        format!(
            "{}/trading-daemon-test-{}-{}.sqlite3",
            std::env::temp_dir().display(),
            label,
            nanos
        )
    }

    #[test]
    fn promotion_updates_snapshot_and_preserves_enable_state() {
        let state_path = unique_state_path("promotion-preserve-enable");
        let db_path = unique_db_path("promotion-preserve-enable");
        let mut state = initial_engine_state(state_path, db_path, 0);
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
        let db_path = unique_db_path("promotion-ttl");
        let mut state = initial_engine_state(state_path, db_path, 100);
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
        let db_path = unique_db_path("promotion-hash");
        let mut state = initial_engine_state(state_path, db_path, 0);
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
        let db_path = unique_db_path("engine-state");
        let mut state = initial_engine_state(state_path, db_path, 0);

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
        let db_path = unique_db_path("snapshot-roundtrip");
        let mut state = initial_engine_state(state_path.clone(), db_path.clone(), 0);
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

        let loaded = initial_engine_state(state_path.clone(), db_path.clone(), 0);
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
        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn default_execution_modes_respect_paper_only_flag() {
        let venues = default_venues();
        let modes = default_execution_modes(&venues);

        assert_eq!(
            modes.get(&mode_key("derivatives_paper", MarketType::Perpetual)),
            Some(&ExecutionMode::Paper)
        );
        assert_eq!(
            modes.get(&mode_key("kalshi", MarketType::Binary)),
            Some(&ExecutionMode::Live)
        );
        assert_eq!(
            modes.get(&mode_key("coinbase_spot", MarketType::Spot)),
            Some(&ExecutionMode::Live)
        );
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
        let state_path = unique_state_path("status-schema");
        let db_path = unique_db_path("status-schema");
        let state = initial_engine_state(state_path.clone(), db_path.clone(), 0);
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

        let _ = std::fs::remove_file(state_path);
        let _ = std::fs::remove_file(db_path);
    }
}
