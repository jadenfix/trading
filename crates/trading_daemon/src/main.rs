use anyhow::{Context, Result};
use bytes::Bytes;
use coinbase_at_adapter::CoinbaseAdvancedTradeAdapter;
use exchange_core::{
    BalanceSnapshot, ExchangeAdapter, FillReport, InstrumentType, NormalizedOrderRequest,
    OpenOrderSnapshot, OrderAck, OrderSnapshot, OrderStatus, PositionSnapshot,
};
use fs2::FileExt;
use futures::{SinkExt, StreamExt};
use risk_core::{HardSafetyCage, HardSafetyPolicy, PromotionRequest, RiskDecision, RiskSnapshot};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, ErrorKind, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use strategy_core::StrategyFamily;
use tokio::net::{UnixListener, UnixStream};
use tokio::signal;
use tokio::sync::Mutex;
use tokio::time::sleep;
use tokio_util::codec::Framed;
use tracing::{error, info, warn};
use trading_protocol::{
    create_codec, CandidatePromotePayload, CandidateUploadPayload, CapabilitiesPayload,
    ControlCommand, DaemonBuildPayload, EngineCommand, EngineMode, EngineModePayload,
    EngineStatePayload, Envelope, Event, ExecutionCommand, ExecutionFillsPayload,
    ExecutionFillsResultPayload, ExecutionGetPayload, ExecutionOpenOrdersPayload,
    ExecutionPlacePayload, ExecutionPlaceResultPayload, PortfolioBalancesPayload, PortfolioCommand,
    PortfolioPositionsPayload, PortfolioSummaryPayload, RequestKind, RiskCommand,
    RiskLimitsPayload, RiskOverridePayload, RiskStatePayload, RoutingCountersPayload,
    ScopedKillSwitchesPayload, StrategyCommand, StrategySummaryPayload, DEFAULT_SOCKET_PATH,
    PROTOCOL_VERSION, STATUS_SCHEMA_VERSION,
};
use uuid::Uuid;

use paper_exchange_adapter::PaperExchangeAdapter;

const DEFAULT_LOCK_PATH: &str = "/var/run/openclaw/trading.lock";
const DEFAULT_DATA_DIR: &str = "/var/lib/openclaw/trading";
const DEFAULT_CANDIDATE_TTL_MS: i64 = 0;
const MAX_RECENT_EVENTS: usize = 128;
const PORTFOLIO_SYNC_INTERVAL_SECS: u64 = 15;
const OPEN_ORDER_RECONCILE_INTERVAL_SECS: u64 = 20;

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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct RoutingCounters {
    live_count: u64,
    paper_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ExecutionStats {
    accepted: u64,
    rejected: u64,
    canceled: u64,
    fills: u64,
}

#[derive(Debug)]
struct EngineState {
    running: bool,
    paused: bool,
    kill_switch_engaged: bool,
    risk_tripped: bool,
    started_at_ms: i64,
    last_command_at_ms: i64,
    mode: EngineMode,
    strategies: HashMap<String, StrategyState>,
    recent_events: VecDeque<serde_json::Value>,
    risk_snapshot: RiskSnapshot,
    safety_policy: HardSafetyPolicy,
    data_dir: String,
    state_path: String,
    candidate_ttl_ms: i64,
    routing_counters: RoutingCounters,
    execution_stats: ExecutionStats,
    scoped_kill_venues: HashSet<String>,
    scoped_kill_strategies: HashSet<String>,
    orders: HashMap<String, OrderSnapshot>,
    fills: Vec<FillReport>,
    processed_intents: HashSet<String>,
    portfolio_positions: Vec<PositionSnapshot>,
    portfolio_balances: Vec<BalanceSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EngineStateSnapshot {
    schema_version: u8,
    saved_at_ms: i64,
    mode: EngineMode,
    kill_switch_engaged: bool,
    paused: bool,
    risk_tripped: bool,
    strategies: Vec<StrategyState>,
    scoped_kill_venues: Vec<String>,
    scoped_kill_strategies: Vec<String>,
    routing_counters: RoutingCounters,
    execution_stats: ExecutionStats,
    orders: Vec<OrderSnapshot>,
    fills: Vec<FillReport>,
    processed_intents: Vec<String>,
    risk_snapshot: RiskSnapshot,
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

type DynAdapter = Arc<dyn ExchangeAdapter>;

#[derive(Clone)]
struct AdapterRegistry {
    coinbase: Option<DynAdapter>,
    paper: DynAdapter,
}

#[derive(Clone)]
struct DaemonContext {
    state: Arc<Mutex<EngineState>>,
    adapters: Arc<AdapterRegistry>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let socket_path = socket_path_from_env();
    let lock_path = lock_path_from_env();
    let data_dir = data_dir_from_env();
    let state_path = state_path_from_env(&data_dir);
    let candidate_ttl_ms = candidate_ttl_ms_from_env();
    let default_mode = mode_from_env();

    info!("Starting trading daemon");
    info!("Socket path: {}", socket_path);
    info!("Lock path: {}", lock_path);
    info!("Data dir: {}", data_dir);
    info!("State path: {}", state_path);
    info!("Default mode: {}", default_mode.as_str());

    ensure_socket_parent_dir(&socket_path)?;
    ensure_data_dirs(&data_dir)?;
    let _lock_file = acquire_single_instance_lock(&lock_path)?;
    let listener = bind_listener(&socket_path)?;

    let adapters = Arc::new(build_adapters());
    let state = Arc::new(Mutex::new(initial_engine_state(
        data_dir,
        state_path,
        candidate_ttl_ms,
        default_mode,
        adapters.coinbase.is_some(),
    )));

    recover_from_todays_journals(&state).await;

    let context = DaemonContext {
        state: Arc::clone(&state),
        adapters: Arc::clone(&adapters),
    };

    spawn_background_reconcilers(context.clone());

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
                        let context = context.clone();
                        tokio::spawn(async move {
                            handle_connection(stream, context).await;
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

fn build_adapters() -> AdapterRegistry {
    let paper: DynAdapter = Arc::new(PaperExchangeAdapter::new("paper"));

    let coinbase = if CoinbaseAdvancedTradeAdapter::credentials_present() {
        match CoinbaseAdvancedTradeAdapter::from_env() {
            Ok(adapter) => Some(Arc::new(adapter) as DynAdapter),
            Err(err) => {
                warn!("Coinbase adapter not initialized: {}", err.message);
                None
            }
        }
    } else {
        warn!("Coinbase credentials missing; live spot execution unavailable");
        None
    };

    AdapterRegistry { coinbase, paper }
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

fn data_dir_from_env() -> String {
    std::env::var("TRADING_DATA_DIR").unwrap_or_else(|_| DEFAULT_DATA_DIR.to_string())
}

fn state_path_from_env(data_dir: &str) -> String {
    std::env::var("TRADING_STATE_PATH")
        .unwrap_or_else(|_| format!("{}/state/engine-state.json", data_dir))
}

fn mode_from_env() -> EngineMode {
    match std::env::var("TRADING_ENGINE_MODE") {
        Ok(mode) => match mode.trim().to_ascii_lowercase().as_str() {
            "paper" => EngineMode::Paper,
            "hitl_live" => EngineMode::HitlLive,
            "auto_live" => EngineMode::AutoLive,
            _ => {
                warn!(
                    "Invalid TRADING_ENGINE_MODE='{}'; defaulting to auto_live",
                    mode
                );
                EngineMode::AutoLive
            }
        },
        Err(_) => EngineMode::AutoLive,
    }
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

fn ensure_data_dirs(data_dir: &str) -> Result<()> {
    std::fs::create_dir_all(Path::new(data_dir).join("state"))
        .with_context(|| format!("Failed to create state dir under {}", data_dir))?;
    std::fs::create_dir_all(Path::new(data_dir).join("journal"))
        .with_context(|| format!("Failed to create journal dir under {}", data_dir))?;
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

fn initial_engine_state(
    data_dir: String,
    state_path: String,
    candidate_ttl_ms: i64,
    mode: EngineMode,
    coinbase_available: bool,
) -> EngineState {
    let now = now_ms();
    let mut state = EngineState {
        running: false,
        paused: false,
        kill_switch_engaged: false,
        risk_tripped: false,
        started_at_ms: now,
        last_command_at_ms: now,
        mode,
        strategies: default_strategies(),
        recent_events: VecDeque::new(),
        risk_snapshot: RiskSnapshot::default(),
        safety_policy: HardSafetyPolicy::default(),
        data_dir,
        state_path,
        candidate_ttl_ms,
        routing_counters: RoutingCounters::default(),
        execution_stats: ExecutionStats::default(),
        scoped_kill_venues: HashSet::new(),
        scoped_kill_strategies: HashSet::new(),
        orders: HashMap::new(),
        fills: Vec::new(),
        processed_intents: HashSet::new(),
        portfolio_positions: Vec::new(),
        portfolio_balances: Vec::new(),
    };

    match load_engine_snapshot(&state.state_path) {
        Ok(Some(snapshot)) => {
            apply_engine_snapshot(&mut state, snapshot);
            info!("Loaded persisted engine state from {}", state.state_path);
        }
        Ok(None) => {}
        Err(err) => {
            warn!(
                "Failed to load persisted engine state from {}: {:#}",
                state.state_path, err
            );
        }
    }

    if state.mode != EngineMode::Paper && !coinbase_available {
        state.running = false;
        state.paused = true;
        state.risk_tripped = true;
        push_event(
            &mut state,
            Event::Alert {
                level: "critical".to_string(),
                message: "auto/hitl live mode requested but Coinbase credentials unavailable; engine remains non-running"
                    .to_string(),
            },
        );
    }

    sync_scoped_kills_into_snapshot(&mut state);
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

fn strategy_snapshot_from_state(state: &EngineState) -> EngineStateSnapshot {
    let mut strategies: Vec<StrategyState> = state.strategies.values().cloned().collect();
    strategies.sort_by(|a, b| a.id.cmp(&b.id));

    EngineStateSnapshot {
        schema_version: 2,
        saved_at_ms: now_ms(),
        mode: state.mode,
        kill_switch_engaged: state.kill_switch_engaged,
        paused: state.paused,
        risk_tripped: state.risk_tripped,
        strategies,
        scoped_kill_venues: state.scoped_kill_venues.iter().cloned().collect(),
        scoped_kill_strategies: state.scoped_kill_strategies.iter().cloned().collect(),
        routing_counters: state.routing_counters.clone(),
        execution_stats: state.execution_stats.clone(),
        orders: state.orders.values().cloned().collect(),
        fills: state.fills.clone(),
        processed_intents: state.processed_intents.iter().cloned().collect(),
        risk_snapshot: state.risk_snapshot.clone(),
    }
}

fn load_engine_snapshot(path: &str) -> Result<Option<EngineStateSnapshot>> {
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err).with_context(|| format!("Failed to read engine snapshot {}", path));
        }
    };

    let snapshot: EngineStateSnapshot = serde_json::from_slice(&raw)
        .with_context(|| format!("Failed to decode engine snapshot {}", path))?;
    Ok(Some(snapshot))
}

fn save_engine_snapshot(path: &str, snapshot: &EngineStateSnapshot) -> Result<()> {
    if let Some(parent) = Path::new(path).parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "Failed to create parent directory for engine snapshot {}",
                parent.display()
            )
        })?;
    }

    let tmp_path = format!("{}.tmp.{}", path, std::process::id());
    let bytes = serde_json::to_vec_pretty(snapshot).context("Failed to encode engine snapshot")?;
    std::fs::write(&tmp_path, bytes)
        .with_context(|| format!("Failed to write temporary engine snapshot {}", tmp_path))?;
    std::fs::rename(&tmp_path, path).with_context(|| {
        format!(
            "Failed to move temporary engine snapshot {} to {}",
            tmp_path, path
        )
    })?;
    Ok(())
}

fn apply_engine_snapshot(state: &mut EngineState, snapshot: EngineStateSnapshot) {
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

    state.mode = snapshot.mode;
    state.kill_switch_engaged = snapshot.kill_switch_engaged;
    state.paused = snapshot.paused;
    state.risk_tripped = snapshot.risk_tripped;
    state.scoped_kill_venues = snapshot.scoped_kill_venues.into_iter().collect();
    state.scoped_kill_strategies = snapshot.scoped_kill_strategies.into_iter().collect();
    state.routing_counters = snapshot.routing_counters;
    state.execution_stats = snapshot.execution_stats;
    state.orders = snapshot
        .orders
        .into_iter()
        .map(|order| (order.venue_order_id.clone(), order))
        .collect();
    state.fills = snapshot.fills;
    state.processed_intents = snapshot.processed_intents.into_iter().collect();
    state.risk_snapshot = snapshot.risk_snapshot;
    sync_scoped_kills_into_snapshot(state);
}

fn persist_engine_state(state: &EngineState) {
    let snapshot = strategy_snapshot_from_state(state);
    if let Err(err) = save_engine_snapshot(&state.state_path, &snapshot) {
        warn!(
            "Failed to persist engine state to {}: {:#}",
            state.state_path, err
        );
    }
}

fn journal_path(data_dir: &str, stream: &str) -> PathBuf {
    let date_key = chrono::Utc::now().format("%Y-%m-%d").to_string();
    Path::new(data_dir)
        .join("journal")
        .join(format!("{}-{}.jsonl", stream, date_key))
}

fn write_journal_entry(data_dir: &str, stream: &str, entry: &serde_json::Value) {
    let path = journal_path(data_dir, stream);
    if let Some(parent) = path.parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            warn!("failed to create journal dir {}: {}", parent.display(), err);
            return;
        }
    }

    let mut file = match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(file) => file,
        Err(err) => {
            warn!("failed to open journal {}: {}", path.display(), err);
            return;
        }
    };

    let line = match serde_json::to_string(entry) {
        Ok(line) => line,
        Err(err) => {
            warn!("failed to serialize journal entry: {}", err);
            return;
        }
    };

    if let Err(err) = writeln!(file, "{}", line) {
        warn!("failed writing journal {}: {}", path.display(), err);
    }
}

async fn recover_from_todays_journals(state: &Arc<Mutex<EngineState>>) {
    let (data_dir, mut orders, mut fills, mut processed) = {
        let state = state.lock().await;
        (
            state.data_dir.clone(),
            HashMap::<String, OrderSnapshot>::new(),
            Vec::<FillReport>::new(),
            HashSet::<String>::new(),
        )
    };

    let order_path = journal_path(&data_dir, "orders");
    if let Ok(file) = File::open(&order_path) {
        let reader = BufReader::new(file);
        for line in reader.lines().map_while(Result::ok) {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };

            if let Some(intent_id) = value.get("intent_id").and_then(serde_json::Value::as_str) {
                processed.insert(intent_id.to_string());
            }

            if let Some(order_value) = value.get("order") {
                if let Ok(order) = serde_json::from_value::<OrderSnapshot>(order_value.clone()) {
                    orders.insert(order.venue_order_id.clone(), order);
                }
            }
        }
    }

    let fill_path = journal_path(&data_dir, "fills");
    if let Ok(file) = File::open(&fill_path) {
        let reader = BufReader::new(file);
        let mut fill_ids = HashSet::new();
        for line in reader.lines().map_while(Result::ok) {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };

            if let Some(fill_value) = value.get("fill") {
                if let Ok(fill) = serde_json::from_value::<FillReport>(fill_value.clone()) {
                    if fill_ids.insert(fill.venue_fill_id.clone()) {
                        fills.push(fill);
                    }
                }
            }
        }
    }

    if !orders.is_empty() || !fills.is_empty() || !processed.is_empty() {
        let mut state = state.lock().await;
        for (k, v) in orders {
            state.orders.insert(k, v);
        }
        state.fills.extend(fills);
        state.processed_intents.extend(processed);
        info!(
            "Recovered state from journal: {} orders, {} fills",
            state.orders.len(),
            state.fills.len()
        );
    }
}

fn sync_scoped_kills_into_snapshot(state: &mut EngineState) {
    state.risk_snapshot.scoped_kill_venues = state.scoped_kill_venues.clone();
    state.risk_snapshot.scoped_kill_strategies = state.scoped_kill_strategies.clone();
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

    sync_scoped_kills_into_snapshot(state);
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

fn route_is_live(
    mode: EngineMode,
    order: &NormalizedOrderRequest,
    adapters: &AdapterRegistry,
) -> bool {
    mode != EngineMode::Paper
        && order.venue == "coinbase_at"
        && order.instrument.instrument_type == InstrumentType::Spot
        && adapters.coinbase.is_some()
}

fn compute_requested_notional_cents(order: &NormalizedOrderRequest) -> i64 {
    if order.requested_notional_cents > 0 {
        return order.requested_notional_cents;
    }

    match order.limit_price {
        Some(price) if price > 0.0 && order.qty > 0.0 => (price * order.qty * 100.0) as i64,
        _ => 0,
    }
}

fn ack_from_order(order: &OrderSnapshot) -> OrderAck {
    OrderAck {
        venue_order_id: order.venue_order_id.clone(),
        client_order_id: order.client_order_id.clone(),
        accepted: !matches!(order.status, OrderStatus::Rejected),
        status: order.status.clone(),
        filled_qty: order.filled_qty,
        avg_fill_price: order.avg_fill_price,
        simulated: order.simulated,
        reason: None,
        ts_ms: order.updated_at_ms,
    }
}

fn synthetic_order_from_ack(order_req: &NormalizedOrderRequest, ack: &OrderAck) -> OrderSnapshot {
    OrderSnapshot {
        venue: order_req.venue.clone(),
        venue_order_id: ack.venue_order_id.clone(),
        client_order_id: ack.client_order_id.clone(),
        strategy_id: order_req.strategy_id.clone(),
        instrument: order_req.instrument.clone(),
        side: order_req.side.clone(),
        order_type: order_req.order_type.clone(),
        status: ack.status.clone(),
        qty: order_req.qty,
        filled_qty: ack.filled_qty,
        limit_price: order_req.limit_price,
        avg_fill_price: ack.avg_fill_price,
        created_at_ms: ack.ts_ms,
        updated_at_ms: ack.ts_ms,
        simulated: ack.simulated,
    }
}

fn maybe_fill_from_ack(order: &OrderSnapshot, ack: &OrderAck) -> Option<FillReport> {
    if ack.filled_qty <= 0.0 {
        return None;
    }

    Some(FillReport {
        venue: order.venue.clone(),
        venue_fill_id: format!("fill-{}", Uuid::new_v4().as_simple()),
        venue_order_id: order.venue_order_id.clone(),
        client_order_id: order.client_order_id.clone(),
        strategy_id: order.strategy_id.clone(),
        instrument: order.instrument.clone(),
        side: order.side.clone(),
        qty: ack.filled_qty,
        price: ack.avg_fill_price.unwrap_or(0.0),
        fee: 0.0,
        fee_asset: order.instrument.quote.clone(),
        liquidity: None,
        simulated: ack.simulated,
        ts_ms: ack.ts_ms,
    })
}

async fn fetch_portfolio(
    adapters: &AdapterRegistry,
) -> (Vec<PositionSnapshot>, Vec<BalanceSnapshot>) {
    let mut positions = Vec::new();
    let mut balances = Vec::new();

    if let Some(coinbase) = &adapters.coinbase {
        match coinbase.sync_positions().await {
            Ok(mut p) => positions.append(&mut p),
            Err(err) => warn!("coinbase sync_positions failed: {}", err.message),
        }
        match coinbase.sync_balances().await {
            Ok(mut b) => balances.append(&mut b),
            Err(err) => warn!("coinbase sync_balances failed: {}", err.message),
        }
    }

    match adapters.paper.sync_positions().await {
        Ok(mut p) => positions.append(&mut p),
        Err(err) => warn!("paper sync_positions failed: {}", err.message),
    }
    match adapters.paper.sync_balances().await {
        Ok(mut b) => balances.append(&mut b),
        Err(err) => warn!("paper sync_balances failed: {}", err.message),
    }

    (positions, balances)
}

fn spawn_background_reconcilers(context: DaemonContext) {
    let portfolio_ctx = context.clone();
    tokio::spawn(async move {
        loop {
            let (positions, balances) = fetch_portfolio(&portfolio_ctx.adapters).await;
            {
                let mut state = portfolio_ctx.state.lock().await;
                state.portfolio_positions = positions;
                state.portfolio_balances = balances;
                let event = Event::PortfolioSync {
                    positions: state.portfolio_positions.len(),
                    balances: state.portfolio_balances.len(),
                };
                push_event(&mut state, event);
                persist_engine_state(&state);
            }
            sleep(Duration::from_secs(PORTFOLIO_SYNC_INTERVAL_SECS)).await;
        }
    });

    tokio::spawn(async move {
        loop {
            let mut open_orders = Vec::<OpenOrderSnapshot>::new();
            if let Some(coinbase) = &context.adapters.coinbase {
                match coinbase.open_orders().await {
                    Ok(mut items) => open_orders.append(&mut items),
                    Err(err) => warn!("coinbase open_orders failed: {}", err.message),
                }
            }
            match context.adapters.paper.open_orders().await {
                Ok(mut items) => open_orders.append(&mut items),
                Err(err) => warn!("paper open_orders failed: {}", err.message),
            }

            {
                let mut state = context.state.lock().await;
                for snapshot in open_orders {
                    state
                        .orders
                        .insert(snapshot.order.venue_order_id.clone(), snapshot.order);
                }
            }

            sleep(Duration::from_secs(OPEN_ORDER_RECONCILE_INTERVAL_SECS)).await;
        }
    });
}

async fn handle_connection(stream: UnixStream, context: DaemonContext) {
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
                let response = process_request(&envelope, &context).await;
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

async fn process_request(request: &Envelope, context: &DaemonContext) -> Envelope {
    match RequestKind::from_kind(request.kind.as_str()) {
        Some(RequestKind::Control(command)) => {
            process_control_request(request, context, command).await
        }
        Some(RequestKind::Engine(command)) => {
            process_engine_request(request, context, command).await
        }
        Some(RequestKind::Strategy(command)) => {
            process_strategy_request(request, context, command).await
        }
        Some(RequestKind::Risk(command)) => process_risk_request(request, context, command).await,
        Some(RequestKind::Execution(command)) => {
            process_execution_request(request, context, command).await
        }
        Some(RequestKind::Portfolio(command)) => {
            process_portfolio_request(request, context, command).await
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
    context: &DaemonContext,
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
            let state = context.state.lock().await;
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
            let mut state = context.state.lock().await;
            if state.kill_switch_engaged {
                return Envelope::response_to(
                    request,
                    json!({
                        "ok": false,
                        "error": "Kill switch engaged; cannot start until operator reset"
                    }),
                );
            }
            if state.mode != EngineMode::Paper && context.adapters.coinbase.is_none() {
                return Envelope::response_to(
                    request,
                    json!({
                        "ok": false,
                        "error": "Coinbase credentials unavailable for live mode"
                    }),
                );
            }

            apply_start(&mut state);
            let event = engine_health_event(&state);
            push_event(&mut state, event);
            persist_engine_state(&state);
            status_response(request, &state)
        }
        ControlCommand::Stop => {
            let mut state = context.state.lock().await;
            apply_stop(&mut state);
            let event = engine_health_event(&state);
            push_event(&mut state, event);
            persist_engine_state(&state);
            status_response(request, &state)
        }
    }
}

async fn process_engine_request(
    request: &Envelope,
    context: &DaemonContext,
    command: EngineCommand,
) -> Envelope {
    match command {
        EngineCommand::Status => {
            let state = context.state.lock().await;
            status_response(request, &state)
        }
        EngineCommand::Pause => {
            let mut state = context.state.lock().await;
            apply_pause(&mut state);
            let event = engine_health_event(&state);
            push_event(&mut state, event);
            persist_engine_state(&state);
            status_response(request, &state)
        }
        EngineCommand::Resume => {
            let mut state = context.state.lock().await;
            if state.kill_switch_engaged {
                return Envelope::response_to(
                    request,
                    json!({
                        "ok": false,
                        "error": "Kill switch engaged; cannot resume"
                    }),
                );
            }
            if state.mode != EngineMode::Paper && context.adapters.coinbase.is_none() {
                return Envelope::response_to(
                    request,
                    json!({
                        "ok": false,
                        "error": "Coinbase credentials unavailable for live mode"
                    }),
                );
            }
            apply_resume(&mut state);
            let event = engine_health_event(&state);
            push_event(&mut state, event);
            persist_engine_state(&state);
            status_response(request, &state)
        }
        EngineCommand::KillSwitch => {
            let mut state = context.state.lock().await;
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
            persist_engine_state(&state);
            status_response(request, &state)
        }
        EngineCommand::GetMode => {
            let state = context.state.lock().await;
            Envelope::response_to(
                request,
                json!({
                    "ok": true,
                    "mode": EngineModePayload { mode: state.mode },
                }),
            )
        }
        EngineCommand::SetMode => {
            let payload: EngineModePayload = match parse_payload(&request.payload) {
                Ok(p) => p,
                Err(err) => {
                    return Envelope::response_to(request, json!({"ok": false, "error": err}));
                }
            };

            if payload.mode != EngineMode::Paper && context.adapters.coinbase.is_none() {
                return Envelope::response_to(
                    request,
                    json!({
                        "ok": false,
                        "error": "Coinbase credentials unavailable for live mode"
                    }),
                );
            }

            let mut state = context.state.lock().await;
            state.mode = payload.mode;
            state.last_command_at_ms = now_ms();
            push_event(
                &mut state,
                Event::Alert {
                    level: "important".to_string(),
                    message: format!("engine mode set to {}", payload.mode.as_str()),
                },
            );
            persist_engine_state(&state);

            Envelope::response_to(
                request,
                json!({
                    "ok": true,
                    "mode": EngineModePayload { mode: state.mode },
                }),
            )
        }
    }
}

async fn process_strategy_request(
    request: &Envelope,
    context: &DaemonContext,
    command: StrategyCommand,
) -> Envelope {
    match command {
        StrategyCommand::List => {
            let state = context.state.lock().await;
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

            let mut state = context.state.lock().await;
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
            persist_engine_state(&state);

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

            let mut state = context.state.lock().await;
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
            persist_engine_state(&state);

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

            let mut state = context.state.lock().await;
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
                    persist_engine_state(&state);

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
    context: &DaemonContext,
    command: RiskCommand,
) -> Envelope {
    match command {
        RiskCommand::Status => {
            let state = context.state.lock().await;
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
            let action = payload.action.clone();
            let venue_for_log = payload.venue.clone();
            let strategy_for_log = payload.strategy_id.clone();

            let mut state = context.state.lock().await;
            match action.as_str() {
                "clear_runtime_counters" => {
                    state.risk_snapshot.orders_last_minute = 0;
                    state.last_command_at_ms = now_ms();
                }
                "reset_kill_switch" | "reset_global" => {
                    apply_reset_kill_switch(&mut state);
                    push_event(
                        &mut state,
                        Event::RiskAlert {
                            level: "important".to_string(),
                            reason: "manual_kill_switch_reset".to_string(),
                            kill_switch_engaged: false,
                        },
                    );
                }
                "kill_global" => {
                    apply_kill_switch(&mut state);
                    push_event(
                        &mut state,
                        Event::RiskAlert {
                            level: "critical".to_string(),
                            reason: "manual_global_kill".to_string(),
                            kill_switch_engaged: true,
                        },
                    );
                }
                "kill_venue" => {
                    let venue = match payload.venue.as_ref() {
                        Some(v) if !v.trim().is_empty() => v.clone(),
                        _ => {
                            return Envelope::response_to(
                                request,
                                json!({ "ok": false, "error": "venue required for kill_venue" }),
                            );
                        }
                    };
                    state.scoped_kill_venues.insert(venue.clone());
                    let kill_switch_engaged = state.kill_switch_engaged;
                    push_event(
                        &mut state,
                        Event::RiskAlert {
                            level: "critical".to_string(),
                            reason: format!("venue kill engaged: {}", venue),
                            kill_switch_engaged,
                        },
                    );
                }
                "reset_venue" => {
                    let venue = match payload.venue.as_ref() {
                        Some(v) if !v.trim().is_empty() => v.clone(),
                        _ => {
                            return Envelope::response_to(
                                request,
                                json!({ "ok": false, "error": "venue required for reset_venue" }),
                            );
                        }
                    };
                    state.scoped_kill_venues.remove(&venue);
                    let kill_switch_engaged = state.kill_switch_engaged;
                    push_event(
                        &mut state,
                        Event::RiskAlert {
                            level: "important".to_string(),
                            reason: format!("venue kill reset: {}", venue),
                            kill_switch_engaged,
                        },
                    );
                }
                "kill_strategy" => {
                    let strategy_id = match payload.strategy_id.as_ref() {
                        Some(v) if !v.trim().is_empty() => v.clone(),
                        _ => {
                            return Envelope::response_to(
                                request,
                                json!({ "ok": false, "error": "strategy_id required for kill_strategy" }),
                            );
                        }
                    };
                    state.scoped_kill_strategies.insert(strategy_id.clone());
                    let kill_switch_engaged = state.kill_switch_engaged;
                    push_event(
                        &mut state,
                        Event::RiskAlert {
                            level: "critical".to_string(),
                            reason: format!("strategy kill engaged: {}", strategy_id),
                            kill_switch_engaged,
                        },
                    );
                }
                "reset_strategy" => {
                    let strategy_id = match payload.strategy_id.as_ref() {
                        Some(v) if !v.trim().is_empty() => v.clone(),
                        _ => {
                            return Envelope::response_to(
                                request,
                                json!({ "ok": false, "error": "strategy_id required for reset_strategy" }),
                            );
                        }
                    };
                    state.scoped_kill_strategies.remove(&strategy_id);
                    let kill_switch_engaged = state.kill_switch_engaged;
                    push_event(
                        &mut state,
                        Event::RiskAlert {
                            level: "important".to_string(),
                            reason: format!("strategy kill reset: {}", strategy_id),
                            kill_switch_engaged,
                        },
                    );
                }
                _ => {
                    return Envelope::response_to(
                        request,
                        json!({
                            "ok": false,
                            "error": "hard safety floor cannot be overridden",
                            "action": action,
                        }),
                    )
                }
            }

            sync_scoped_kills_into_snapshot(&mut state);
            persist_engine_state(&state);

            write_journal_entry(
                &state.data_dir,
                "risk",
                &json!({
                    "ts_ms": now_ms(),
                    "action": action,
                    "venue": venue_for_log,
                    "strategy_id": strategy_for_log,
                    "state": risk_state_payload(&state),
                }),
            );

            Envelope::response_to(
                request,
                json!({
                    "ok": true,
                    "action": action,
                    "state": risk_state_payload(&state),
                    "note": "Engine remains PAUSED after kill switch reset. Use 'resume' to start.",
                }),
            )
        }
    }
}

async fn process_execution_request(
    request: &Envelope,
    context: &DaemonContext,
    command: ExecutionCommand,
) -> Envelope {
    match command {
        ExecutionCommand::Place => {
            let payload: ExecutionPlacePayload = match parse_payload(&request.payload) {
                Ok(p) => p,
                Err(err) => {
                    return Envelope::response_to(request, json!({"ok": false, "error": err}));
                }
            };

            let order = payload.order;
            let intent_id = order
                .intent_id
                .clone()
                .unwrap_or_else(|| order.client_order_id.clone());

            {
                let state = context.state.lock().await;
                if state.processed_intents.contains(&intent_id) {
                    if let Some(existing) = state
                        .orders
                        .values()
                        .find(|o| o.client_order_id == order.client_order_id)
                        .cloned()
                    {
                        let ack = ack_from_order(&existing);
                        return Envelope::response_to(
                            request,
                            json!({
                                "ok": true,
                                "idempotent_replay": true,
                                "result": ExecutionPlaceResultPayload {
                                    ack,
                                    order: Some(existing),
                                    fill: None,
                                }
                            }),
                        );
                    }
                }
            }

            let (mode, running, paused, requested_notional_cents, risk_snapshot, safety_policy) = {
                let state = context.state.lock().await;
                (
                    state.mode,
                    state.running,
                    state.paused,
                    compute_requested_notional_cents(&order),
                    state.risk_snapshot.clone(),
                    state.safety_policy.clone(),
                )
            };

            if !running || paused {
                return Envelope::response_to(
                    request,
                    json!({"ok": false, "error": "engine is not running"}),
                );
            }

            let missing_approval = payload
                .approval_token
                .as_ref()
                .map(|token| token.trim().is_empty())
                .unwrap_or(true);
            if mode == EngineMode::HitlLive && missing_approval {
                return Envelope::response_to(
                    request,
                    json!({"ok": false, "error": "approval_token required in hitl_live mode"}),
                );
            }

            if requested_notional_cents <= 0 {
                return Envelope::response_to(
                    request,
                    json!({"ok": false, "error": "requested_notional_cents must be positive"}),
                );
            }

            let cage = HardSafetyCage::new(safety_policy);
            let risk_decision = cage.evaluate_order_with_scope(
                &order.strategy_id,
                &order.venue,
                &order.instrument.asset_class,
                requested_notional_cents,
                &risk_snapshot,
            );

            if let RiskDecision::Deny { reason } = risk_decision {
                let mut state = context.state.lock().await;
                state.execution_stats.rejected = state.execution_stats.rejected.saturating_add(1);
                let kill_switch_engaged = state.kill_switch_engaged;
                push_event(
                    &mut state,
                    Event::RiskAlert {
                        level: "warning".to_string(),
                        reason: format!("execution denied: {}", reason),
                        kill_switch_engaged,
                    },
                );
                persist_engine_state(&state);

                return Envelope::response_to(
                    request,
                    json!({"ok": false, "error": reason, "hard_safety_floor": "enforced"}),
                );
            }

            let live_route = route_is_live(mode, &order, &context.adapters);
            let (adapter, routed_to) = if live_route {
                match &context.adapters.coinbase {
                    Some(adapter) => (adapter.clone(), "coinbase_at"),
                    None => {
                        return Envelope::response_to(
                            request,
                            json!({"ok": false, "error": "live route requested but coinbase adapter unavailable"}),
                        );
                    }
                }
            } else {
                (context.adapters.paper.clone(), "paper")
            };

            let ack = match adapter.place_order(order.clone()).await {
                Ok(ack) => ack,
                Err(err) => {
                    let mut state = context.state.lock().await;
                    state.execution_stats.rejected =
                        state.execution_stats.rejected.saturating_add(1);
                    push_event(
                        &mut state,
                        Event::Execution {
                            venue: order.venue.clone(),
                            strategy_id: order.strategy_id.clone(),
                            symbol: order.symbol.clone(),
                            action: "place".to_string(),
                            status: "failed".to_string(),
                            latency_ms: 0,
                            simulated: routed_to == "paper",
                            venue_order_id: None,
                        },
                    );
                    persist_engine_state(&state);

                    return Envelope::response_to(
                        request,
                        json!({"ok": false, "error": err.message, "code": err.code}),
                    );
                }
            };

            let order_snapshot = match adapter.get_order(&ack.venue_order_id).await {
                Ok(Some(snapshot)) => snapshot,
                Ok(None) => synthetic_order_from_ack(&order, &ack),
                Err(_) => synthetic_order_from_ack(&order, &ack),
            };
            let maybe_fill = maybe_fill_from_ack(&order_snapshot, &ack);

            let mut state = context.state.lock().await;
            state.execution_stats.accepted = state.execution_stats.accepted.saturating_add(1);
            if maybe_fill.is_some() {
                state.execution_stats.fills = state.execution_stats.fills.saturating_add(1);
            }

            if live_route {
                state.routing_counters.live_count =
                    state.routing_counters.live_count.saturating_add(1);
            } else {
                state.routing_counters.paper_count =
                    state.routing_counters.paper_count.saturating_add(1);
            }

            state.risk_snapshot.total_notional_cents = state
                .risk_snapshot
                .total_notional_cents
                .saturating_add(requested_notional_cents);
            state.risk_snapshot.orders_last_minute =
                state.risk_snapshot.orders_last_minute.saturating_add(1);
            *state
                .risk_snapshot
                .strategy_canary_notional
                .entry(order.strategy_id.clone())
                .or_insert(0) += requested_notional_cents;
            *state
                .risk_snapshot
                .venue_notional
                .entry(order.venue.clone())
                .or_insert(0) += requested_notional_cents;
            *state
                .risk_snapshot
                .asset_class_notional
                .entry(order.instrument.asset_class.clone())
                .or_insert(0) += requested_notional_cents;

            state.orders.insert(
                order_snapshot.venue_order_id.clone(),
                order_snapshot.clone(),
            );
            if let Some(fill) = &maybe_fill {
                state.fills.push(fill.clone());
            }
            state.processed_intents.insert(intent_id.clone());
            state.last_command_at_ms = now_ms();

            push_event(
                &mut state,
                Event::Execution {
                    venue: order.venue.clone(),
                    strategy_id: order.strategy_id.clone(),
                    symbol: order.symbol.clone(),
                    action: "place".to_string(),
                    status: "accepted".to_string(),
                    latency_ms: 0,
                    simulated: ack.simulated,
                    venue_order_id: Some(ack.venue_order_id.clone()),
                },
            );

            write_journal_entry(
                &state.data_dir,
                "orders",
                &json!({
                    "ts_ms": now_ms(),
                    "intent_id": intent_id,
                    "routed_to": routed_to,
                    "order": order_snapshot,
                }),
            );
            if let Some(fill) = &maybe_fill {
                write_journal_entry(
                    &state.data_dir,
                    "fills",
                    &json!({
                        "ts_ms": now_ms(),
                        "intent_id": order.intent_id,
                        "fill": fill,
                    }),
                );
            }

            persist_engine_state(&state);

            Envelope::response_to(
                request,
                json!({
                    "ok": true,
                    "routed_to": routed_to,
                    "result": ExecutionPlaceResultPayload {
                        ack,
                        order: Some(order_snapshot),
                        fill: maybe_fill,
                    }
                }),
            )
        }
        ExecutionCommand::Cancel => {
            let payload: trading_protocol::ExecutionCancelPayload =
                match parse_payload(&request.payload) {
                    Ok(p) => p,
                    Err(err) => {
                        return Envelope::response_to(request, json!({"ok": false, "error": err}));
                    }
                };

            let order = {
                let state = context.state.lock().await;
                state.orders.get(&payload.venue_order_id).cloned()
            };

            let adapter = if order.as_ref().is_some_and(|o| {
                o.venue == "coinbase_at" && o.instrument.instrument_type == InstrumentType::Spot
            }) {
                match &context.adapters.coinbase {
                    Some(adapter) => adapter.clone(),
                    None => {
                        return Envelope::response_to(
                            request,
                            json!({"ok": false, "error": "coinbase adapter unavailable"}),
                        )
                    }
                }
            } else {
                context.adapters.paper.clone()
            };

            if let Err(err) = adapter.cancel_order(&payload.venue_order_id).await {
                return Envelope::response_to(
                    request,
                    json!({"ok": false, "error": err.message, "code": err.code}),
                );
            }

            let mut state = context.state.lock().await;
            if let Some(existing) = state.orders.get_mut(&payload.venue_order_id) {
                existing.status = OrderStatus::Canceled;
                existing.updated_at_ms = now_ms();
            }
            state.execution_stats.canceled = state.execution_stats.canceled.saturating_add(1);
            persist_engine_state(&state);

            Envelope::response_to(
                request,
                json!({"ok": true, "venue_order_id": payload.venue_order_id}),
            )
        }
        ExecutionCommand::Get => {
            let payload: ExecutionGetPayload = match parse_payload(&request.payload) {
                Ok(p) => p,
                Err(err) => {
                    return Envelope::response_to(request, json!({"ok": false, "error": err}));
                }
            };

            let state = context.state.lock().await;
            let order = state.orders.get(&payload.venue_order_id).cloned();
            Envelope::response_to(
                request,
                json!({
                    "ok": true,
                    "order": order,
                }),
            )
        }
        ExecutionCommand::OpenOrders => {
            let mut orders = Vec::new();
            if let Some(adapter) = &context.adapters.coinbase {
                if let Ok(mut o) = adapter.open_orders().await {
                    orders.append(&mut o);
                }
            }
            if let Ok(mut o) = context.adapters.paper.open_orders().await {
                orders.append(&mut o);
            }

            Envelope::response_to(
                request,
                json!({
                    "ok": true,
                    "result": ExecutionOpenOrdersPayload { orders },
                }),
            )
        }
        ExecutionCommand::Fills => {
            let payload: ExecutionFillsPayload =
                parse_payload(&request.payload).unwrap_or(ExecutionFillsPayload {
                    since_ts_ms: Some(0),
                    limit: Some(200),
                });
            let since_ts_ms = payload.since_ts_ms.unwrap_or(0);
            let limit = payload.limit.unwrap_or(200);

            let state = context.state.lock().await;
            let mut fills: Vec<FillReport> = state
                .fills
                .iter()
                .filter(|fill| fill.ts_ms >= since_ts_ms)
                .cloned()
                .collect();
            fills.sort_by_key(|fill| fill.ts_ms);
            if fills.len() > limit {
                fills = fills[fills.len().saturating_sub(limit)..].to_vec();
            }

            Envelope::response_to(
                request,
                json!({
                    "ok": true,
                    "result": ExecutionFillsResultPayload { fills },
                }),
            )
        }
    }
}

async fn process_portfolio_request(
    request: &Envelope,
    context: &DaemonContext,
    command: PortfolioCommand,
) -> Envelope {
    match command {
        PortfolioCommand::Positions => {
            let (positions, balances) = fetch_portfolio(&context.adapters).await;
            {
                let mut state = context.state.lock().await;
                state.portfolio_positions = positions.clone();
                state.portfolio_balances = balances;
            }

            Envelope::response_to(
                request,
                json!({
                    "ok": true,
                    "result": PortfolioPositionsPayload { positions },
                }),
            )
        }
        PortfolioCommand::Balances => {
            let (positions, balances) = fetch_portfolio(&context.adapters).await;
            {
                let mut state = context.state.lock().await;
                state.portfolio_positions = positions;
                state.portfolio_balances = balances.clone();
            }

            Envelope::response_to(
                request,
                json!({
                    "ok": true,
                    "result": PortfolioBalancesPayload { balances },
                }),
            )
        }
        PortfolioCommand::Exposure => {
            let state = context.state.lock().await;
            Envelope::response_to(
                request,
                json!({
                    "ok": true,
                    "result": portfolio_summary_payload(&state),
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

fn scoped_kill_switches_payload(state: &EngineState) -> ScopedKillSwitchesPayload {
    let mut venues: Vec<String> = state.scoped_kill_venues.iter().cloned().collect();
    let mut strategies: Vec<String> = state.scoped_kill_strategies.iter().cloned().collect();
    venues.sort();
    strategies.sort();

    ScopedKillSwitchesPayload {
        global: state.kill_switch_engaged,
        venues,
        strategies,
    }
}

fn risk_state_payload(state: &EngineState) -> RiskStatePayload {
    RiskStatePayload {
        kill_switch_engaged: state.kill_switch_engaged,
        paused: state.paused,
        orders_last_minute: state.risk_snapshot.orders_last_minute,
        drawdown_cents: state.risk_snapshot.drawdown_cents,
        total_notional_cents: state.risk_snapshot.total_notional_cents,
        scoped_kill_switches: scoped_kill_switches_payload(state),
    }
}

fn portfolio_summary_payload(state: &EngineState) -> PortfolioSummaryPayload {
    let mut by_venue: Vec<trading_protocol::VenueExposurePayload> = state
        .risk_snapshot
        .venue_notional
        .iter()
        .map(|(venue, notional)| trading_protocol::VenueExposurePayload {
            venue: venue.clone(),
            notional_cents: *notional,
        })
        .collect();
    by_venue.sort_by(|a, b| a.venue.cmp(&b.venue));

    let mut by_asset_class: Vec<trading_protocol::AssetClassExposurePayload> = state
        .risk_snapshot
        .asset_class_notional
        .iter()
        .map(
            |(asset_class, notional)| trading_protocol::AssetClassExposurePayload {
                asset_class: asset_class.clone(),
                notional_cents: *notional,
            },
        )
        .collect();
    by_asset_class
        .sort_by(|a, b| format!("{:?}", a.asset_class).cmp(&format!("{:?}", b.asset_class)));

    PortfolioSummaryPayload {
        total_notional_cents: state.risk_snapshot.total_notional_cents,
        by_venue,
        by_asset_class,
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
        mode: state.mode,
        routing_counters: RoutingCountersPayload {
            live_count: state.routing_counters.live_count,
            paper_count: state.routing_counters.paper_count,
        },
        scoped_kill_switches: scoped_kill_switches_payload(state),
        execution_stats: trading_protocol::ExecutionStatsPayload {
            accepted: state.execution_stats.accepted,
            rejected: state.execution_stats.rejected,
            canceled: state.execution_stats.canceled,
            fills: state.execution_stats.fills,
        },
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
        EngineCommand::GetMode.as_kind().to_string(),
        EngineCommand::SetMode.as_kind().to_string(),
        StrategyCommand::List.as_kind().to_string(),
        StrategyCommand::Enable.as_kind().to_string(),
        StrategyCommand::Disable.as_kind().to_string(),
        StrategyCommand::UploadCandidate.as_kind().to_string(),
        StrategyCommand::PromoteCandidate.as_kind().to_string(),
        RiskCommand::Status.as_kind().to_string(),
        RiskCommand::Override.as_kind().to_string(),
        ExecutionCommand::Place.as_kind().to_string(),
        ExecutionCommand::Cancel.as_kind().to_string(),
        ExecutionCommand::Get.as_kind().to_string(),
        ExecutionCommand::OpenOrders.as_kind().to_string(),
        ExecutionCommand::Fills.as_kind().to_string(),
        PortfolioCommand::Positions.as_kind().to_string(),
        PortfolioCommand::Balances.as_kind().to_string(),
        PortfolioCommand::Exposure.as_kind().to_string(),
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
            "portfolio_summary": portfolio_summary_payload(state),
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

    state.recent_events.push_back(value.clone());
    while state.recent_events.len() > MAX_RECENT_EVENTS {
        state.recent_events.pop_front();
    }

    write_journal_entry(
        &state.data_dir,
        "events",
        &json!({
            "ts_ms": now_ms(),
            "event": value,
        }),
    );
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
        let mut state = initial_engine_state(
            std::env::temp_dir().to_string_lossy().to_string(),
            state_path,
            0,
            EngineMode::Paper,
            true,
        );
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
        let mut state = initial_engine_state(
            std::env::temp_dir().to_string_lossy().to_string(),
            state_path,
            100,
            EngineMode::Paper,
            true,
        );
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
    fn route_is_live_only_for_spot_coinbase_and_live_mode() {
        let adapters = AdapterRegistry {
            coinbase: Some(Arc::new(PaperExchangeAdapter::new("coinbase_at")) as DynAdapter),
            paper: Arc::new(PaperExchangeAdapter::new("paper")),
        };

        let order = NormalizedOrderRequest {
            venue: "coinbase_at".to_string(),
            symbol: "BTC-USD".to_string(),
            instrument: exchange_core::InstrumentRef {
                venue: "coinbase_at".to_string(),
                venue_symbol: "BTC-USD".to_string(),
                asset_class: exchange_core::AssetClass::Crypto,
                instrument_type: InstrumentType::Spot,
                base: Some("BTC".to_string()),
                quote: Some("USD".to_string()),
                expiry_ts_ms: None,
                strike: None,
                option_right: None,
                contract_multiplier: Some(1.0),
            },
            strategy_id: "s1".to_string(),
            client_order_id: "c1".to_string(),
            intent_id: Some("i1".to_string()),
            side: exchange_core::OrderSide::Buy,
            order_type: exchange_core::OrderType::Limit,
            qty: 1.0,
            limit_price: Some(100.0),
            tif: Some(exchange_core::TimeInForce::Gtc),
            post_only: false,
            reduce_only: false,
            requested_notional_cents: 10_000,
        };

        assert!(route_is_live(EngineMode::AutoLive, &order, &adapters));
        assert!(!route_is_live(EngineMode::Paper, &order, &adapters));
    }
}
