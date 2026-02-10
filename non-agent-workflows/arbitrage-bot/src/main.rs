//! Arbitrage Bot Entry Point.
//!
//! Orchestrates the tasks:
//! 1. Universe Discovery (periodic)
//! 2. WebSocket stream (continuous)
//! 3. Arb Evaluation Loop (continuous)

mod config;

use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{
    collections::HashSet,
    fs::{create_dir_all, File, OpenOptions},
    hash::{Hash, Hasher},
    io::Write,
    path::{Path, PathBuf},
};

use arb_strategy::{
    ArbDirection, ArbEvaluator, ArbExecutor, ArbFeeModel, ArbOpportunity, ArbRiskManager,
    ExecutionOutcome, QuoteBook, UniverseBuilder,
};
use chrono::{SecondsFormat, Utc};
use clap::Parser;
use kalshi_client::{new_price_cache, KalshiAuth, KalshiRestClient, KalshiWsClient, PriceEntry};
use serde_json::json;
use tokio::sync::RwLock;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

use crate::config::load_config;

#[derive(Parser)]
struct Cli {
    #[arg(long)]
    check_auth: bool,

    #[arg(long)]
    dry_run: bool,
}

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

fn opportunity_fingerprint(opp: &ArbOpportunity) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    opp.group_event_ticker.hash(&mut hasher);
    match opp.direction {
        ArbDirection::BuySet => "buy".hash(&mut hasher),
        ArbDirection::SellSet => "sell".hash(&mut hasher),
    }
    opp.qty.hash(&mut hasher);
    opp.net_edge_cents.hash(&mut hasher);
    for leg in &opp.legs {
        leg.ticker.hash(&mut hasher);
        leg.price_cents.hash(&mut hasher);
        match leg.side {
            common::Side::Yes => "yes".hash(&mut hasher),
            common::Side::No => "no".hash(&mut hasher),
        }
    }
    hasher.finish()
}

fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn resolve_repo_root() -> Option<PathBuf> {
    let mut cursor = std::env::current_dir().ok()?;
    loop {
        if cursor.join(".git").is_dir() {
            return Some(cursor);
        }
        if !cursor.pop() {
            return None;
        }
    }
}

fn resolve_trades_dir() -> PathBuf {
    if let Ok(raw) = std::env::var("TRADES_DIR") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }

    if let Some(root) = resolve_repo_root() {
        return root.join("TRADES");
    }

    PathBuf::from("TRADES")
}

fn direction_label(direction: ArbDirection) -> &'static str {
    match direction {
        ArbDirection::BuySet => "buy_set",
        ArbDirection::SellSet => "sell_set",
    }
}

fn side_label(side: common::Side) -> &'static str {
    match side {
        common::Side::Yes => "yes",
        common::Side::No => "no",
    }
}

fn outcome_label(outcome: ExecutionOutcome) -> &'static str {
    match outcome {
        ExecutionOutcome::CompleteFill => "complete_fill",
        ExecutionOutcome::NoFill => "no_fill",
        ExecutionOutcome::PartialFillUnwound => "partial_fill_unwound",
        ExecutionOutcome::PartialFillUnwindFailed => "partial_fill_unwind_failed",
        ExecutionOutcome::Rejected => "rejected",
        ExecutionOutcome::Failed => "failed",
    }
}

fn legs_json(opp: &ArbOpportunity) -> Vec<serde_json::Value> {
    opp.legs
        .iter()
        .map(|leg| {
            json!({
                "ticker": leg.ticker,
                "side": side_label(leg.side),
                "price_cents": leg.price_cents
            })
        })
        .collect()
}

struct TradeJournal {
    dir: PathBuf,
    day_key: String,
    file: File,
}

impl TradeJournal {
    fn open(dir: PathBuf) -> std::io::Result<Self> {
        create_dir_all(&dir)?;
        let day_key = Utc::now().format("%Y-%m-%d").to_string();
        let file = Self::open_day_file(&dir, &day_key)?;
        Ok(Self { dir, day_key, file })
    }

    fn open_day_file(dir: &Path, day_key: &str) -> std::io::Result<File> {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(format!("trades-{}.jsonl", day_key)))
    }

    fn rotate_if_needed(&mut self) -> std::io::Result<()> {
        let today = Utc::now().format("%Y-%m-%d").to_string();
        if today != self.day_key {
            self.file = Self::open_day_file(&self.dir, &today)?;
            self.day_key = today;
        }
        Ok(())
    }

    fn write_event(&mut self, event: serde_json::Value) {
        let write_result = (|| -> std::io::Result<()> {
            self.rotate_if_needed()?;
            let line = serde_json::to_string(&event).unwrap_or_else(|_| "{}".to_string());
            writeln!(self.file, "{}", line)?;
            self.file.flush()?;
            Ok(())
        })();

        if let Err(e) = write_result {
            warn!("Trade journal write failed: {}", e);
        }
    }

    fn dir(&self) -> &Path {
        &self.dir
    }
}

fn write_trade_event(journal: &mut TradeJournal, event: serde_json::Value) {
    journal.write_event(event);
}

async fn fetch_balance_with_retry(
    client: &KalshiRestClient,
    max_attempts: u32,
    backoff: Duration,
) -> Option<i64> {
    for attempt in 1..=max_attempts {
        match client.get_balance().await {
            Ok(balance) => return Some(balance),
            Err(e) => {
                if attempt == max_attempts {
                    error!(
                        "Balance preflight failed after {} attempts: {}",
                        max_attempts, e
                    );
                } else {
                    warn!(
                        "Balance preflight attempt {}/{} failed: {}",
                        attempt, max_attempts, e
                    );
                    sleep(backoff).await;
                }
            }
        }
    }
    None
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "arbitrage_bot=info,arb_strategy=info,kalshi_client=info".into()
            }),
        )
        .init();

    info!("🚀 Arbitrage Bot starting...");

    let cli = Cli::parse();
    if cli.dry_run {
        info!("Dry-run mode enabled: opportunities will be logged but not executed.");
    }
    let cfg = match load_config() {
        Ok(c) => c,
        Err(e) => {
            error!("Config error: {}", e);
            return;
        }
    };
    info!(
        "Environment: {}",
        if cfg.use_demo { "DEMO" } else { "PRODUCTION" }
    );
    info!(
        "Arb config: min_profit={}¢ slippage={}¢ guaranteed_only={} strict_binary_only={} ev_mode={}",
        cfg.arb.min_profit_cents,
        cfg.arb.slippage_buffer_cents,
        cfg.arb.guaranteed_arb_only,
        cfg.arb.strict_binary_only,
        cfg.arb.ev_mode_enabled
    );
    if cfg.timing.quote_stale_secs < 10 {
        warn!(
            "quote_stale_secs={} is very low; many markets may be skipped as stale",
            cfg.timing.quote_stale_secs
        );
    }

    let trades_dir = resolve_trades_dir();
    let mut trade_journal = match TradeJournal::open(trades_dir) {
        Ok(journal) => journal,
        Err(e) => {
            error!("Failed to initialize trade journal: {}", e);
            return;
        }
    };
    info!("Trade journal path: {}", trade_journal.dir().display());
    write_trade_event(
        &mut trade_journal,
        json!({
            "ts": now_iso(),
            "kind": "bot_start",
            "mode": if cli.dry_run { "dry_run" } else { "live" },
            "use_demo": cfg.use_demo,
            "arb": {
                "min_profit_cents": cfg.arb.min_profit_cents,
                "slippage_buffer_cents": cfg.arb.slippage_buffer_cents,
                "guaranteed_arb_only": cfg.arb.guaranteed_arb_only,
                "strict_binary_only": cfg.arb.strict_binary_only,
                "ev_mode_enabled": cfg.arb.ev_mode_enabled,
                "default_qty": cfg.arb.default_qty
            },
            "timing": {
                "eval_interval_ms": cfg.timing.eval_interval_ms,
                "quote_stale_secs": cfg.timing.quote_stale_secs,
                "universe_refresh_secs": cfg.timing.universe_refresh_secs
            }
        }),
    );

    let auth = match KalshiAuth::new(&cfg.api_key, &cfg.secret_key) {
        Ok(a) => a,
        Err(e) => {
            error!("Auth init failed: {}", e);
            return;
        }
    };

    let rest_client = KalshiRestClient::new(auth.clone(), cfg.use_demo);

    // Auth check.
    if cli.check_auth {
        match rest_client.get_balance().await {
            Ok(bal) => {
                info!("✅ Auth valid. Balance: {}¢", bal);
                write_trade_event(
                    &mut trade_journal,
                    json!({
                        "ts": now_iso(),
                        "kind": "auth_check",
                        "status": "ok",
                        "balance_cents": bal
                    }),
                );
            }
            Err(e) => {
                error!("❌ Auth failed: {}", e);
                write_trade_event(
                    &mut trade_journal,
                    json!({
                        "ts": now_iso(),
                        "kind": "auth_check",
                        "status": "error",
                        "error": e.to_string()
                    }),
                );
            }
        }
        return;
    }

    // Shared state.
    let price_cache = new_price_cache(); // Shared with WS
    let quote_book = QuoteBook::new(price_cache.clone()); // Wrapper for arb strategy

    let universe = Arc::new(RwLock::new(Vec::new()));
    let tickers_to_track = Arc::new(RwLock::new(Vec::new()));

    // Task 1: WebSocket Client.
    let ws_client = KalshiWsClient::new(auth.clone(), cfg.use_demo, price_cache.clone());
    let ws_tickers = tickers_to_track.clone();

    tokio::spawn(async move {
        ws_client.run(ws_tickers).await;
    });

    // Task 2: Universe Discovery.
    let disc_client = rest_client.clone();
    let disc_universe = universe.clone();
    let disc_tickers = tickers_to_track.clone();
    let disc_price_cache = price_cache.clone();
    let refresh_secs = cfg.timing.universe_refresh_secs;

    tokio::spawn(async move {
        loop {
            let groups = UniverseBuilder::build(&disc_client).await;

            // Collect all unique tickers to update WS subscription.
            let mut all_tickers = Vec::new();
            for g in &groups {
                all_tickers.extend(g.tickers.clone());
            }
            // Dedup.
            all_tickers.sort();
            all_tickers.dedup();

            // Seed missing quote entries from REST snapshot so strategy
            // can evaluate immediately even before WS ticker deltas arrive.
            let mut seeded_quotes = 0usize;
            {
                let mut cache = disc_price_cache.write().await;
                let now = Instant::now();

                for group in &groups {
                    for market in group.markets.values() {
                        cache.entry(market.ticker.clone()).or_insert_with(|| {
                            seeded_quotes += 1;
                            PriceEntry {
                                yes_bid: market.yes_bid,
                                yes_ask: market.yes_ask,
                                last_price: market.last_price,
                                volume: market.volume,
                                updated_at: now,
                            }
                        });
                    }
                }
            }

            info!(
                "Universe updated: {} groups, {} unique tickers, {} REST-seeded quotes",
                groups.len(),
                all_tickers.len(),
                seeded_quotes
            );

            // Update shared state.
            *disc_universe.write().await = groups;
            *disc_tickers.write().await = all_tickers;

            sleep(Duration::from_secs(refresh_secs)).await;
        }
    });

    // Task 3: Strategy Loop.
    let strategy_universe = universe.clone();
    let fees = ArbFeeModel::new(cfg.arb.slippage_buffer_cents);
    let mut risk = ArbRiskManager::new(cfg.risk.clone());
    let exec = ArbExecutor::new(&rest_client, &fees, &cfg.execution);
    let mut last_balance_refresh = Instant::now();
    let mut last_balance_ok: Option<Instant> = None;

    if !cli.dry_run {
        match fetch_balance_with_retry(&rest_client, 3, Duration::from_secs(2)).await {
            Some(balance) => {
                if let Err(e) = risk.update_balance(balance) {
                    error!("Startup risk gate failed: {}", e);
                    write_trade_event(
                        &mut trade_journal,
                        json!({
                            "ts": now_iso(),
                            "kind": "startup_risk_gate",
                            "status": "error",
                            "balance_cents": balance,
                            "error": e.to_string()
                        }),
                    );
                    return;
                }
                info!("Pre-trade balance check passed: {}¢", balance);
                last_balance_ok = Some(Instant::now());
                write_trade_event(
                    &mut trade_journal,
                    json!({
                        "ts": now_iso(),
                        "kind": "startup_balance_check",
                        "status": "ok",
                        "balance_cents": balance
                    }),
                );
            }
            None => {
                error!("Unable to fetch account balance; refusing live execution.");
                write_trade_event(
                    &mut trade_journal,
                    json!({
                        "ts": now_iso(),
                        "kind": "startup_balance_check",
                        "status": "error",
                        "error": "unable to fetch account balance"
                    }),
                );
                return;
            }
        }
    }

    // Wait for initial universe.
    info!("Waiting for universe discovery...");
    let mut wait_log_timer = Instant::now();
    while strategy_universe.read().await.is_empty() {
        if wait_log_timer.elapsed() >= Duration::from_secs(15) {
            info!("Still waiting for initial universe discovery...");
            write_trade_event(
                &mut trade_journal,
                json!({
                    "ts": now_iso(),
                    "kind": "universe_waiting",
                    "status": "pending"
                }),
            );
            wait_log_timer = Instant::now();
        }
        sleep(Duration::from_secs(2)).await;
    }

    info!("Starting evaluation loop...");
    let mut prev_cycle_opps: HashSet<u64> = HashSet::new();
    let mut last_heartbeat = Instant::now();
    let mut cycles_since_heartbeat: u64 = 0;
    let mut new_opps_since_heartbeat: usize = 0;
    let mut execution_attempts_since_heartbeat: usize = 0;
    let mut risk_rejects_since_heartbeat: usize = 0;
    let mut execution_failures_since_heartbeat: usize = 0;

    loop {
        cycles_since_heartbeat = cycles_since_heartbeat.saturating_add(1);

        if !cli.dry_run && last_balance_refresh.elapsed() >= Duration::from_secs(30) {
            last_balance_refresh = Instant::now();
            match rest_client.get_balance().await {
                Ok(balance) => {
                    last_balance_ok = Some(Instant::now());
                    if let Err(e) = risk.update_balance(balance) {
                        error!("Risk kill switch engaged: {}", e);
                        write_trade_event(
                            &mut trade_journal,
                            json!({
                                "ts": now_iso(),
                                "kind": "risk_kill_switch",
                                "reason": "balance_guard",
                                "balance_cents": balance,
                                "error": e.to_string()
                            }),
                        );
                        return;
                    }
                    debug!("Balance heartbeat: {}¢", balance);
                    write_trade_event(
                        &mut trade_journal,
                        json!({
                            "ts": now_iso(),
                            "kind": "balance_heartbeat",
                            "status": "ok",
                            "balance_cents": balance
                        }),
                    );
                }
                Err(e) => {
                    warn!("Balance heartbeat failed: {}", e);
                    write_trade_event(
                        &mut trade_journal,
                        json!({
                            "ts": now_iso(),
                            "kind": "balance_heartbeat",
                            "status": "error",
                            "error": e.to_string()
                        }),
                    );
                }
            }
        }
        if risk.kill_switch_engaged() {
            error!("Kill switch is engaged. Stopping execution loop.");
            write_trade_event(
                &mut trade_journal,
                json!({
                    "ts": now_iso(),
                    "kind": "risk_kill_switch",
                    "reason": "explicit_state",
                }),
            );
            return;
        }

        let groups = strategy_universe.read().await.clone();
        let group_count = groups.len();
        let mut current_cycle_opps: HashSet<u64> = HashSet::new();
        let mut new_opp_count = 0usize;

        let evaluator = ArbEvaluator::new(&fees, &quote_book);

        for group in groups {
            // Evaluate.
            if let Some(opp) = evaluator
                .evaluate(
                    &group,
                    cfg.arb.min_profit_cents,
                    cfg.arb.ev_mode_enabled,
                    cfg.arb.guaranteed_arb_only,
                    cfg.arb.strict_binary_only,
                    cfg.arb.tie_buffer_cents,
                    cfg.timing.quote_stale_secs,
                    cfg.arb.default_qty,
                )
                .await
            {
                let fingerprint = opportunity_fingerprint(&opp);
                current_cycle_opps.insert(fingerprint);
                let is_new = !prev_cycle_opps.contains(&fingerprint);

                if is_new {
                    new_opp_count += 1;
                    new_opps_since_heartbeat = new_opps_since_heartbeat.saturating_add(1);
                    info!(
                        "OPPORTUNITY FOUND: event={} direction={:?} legs={} net={}¢ reason={}",
                        opp.group_event_ticker,
                        opp.direction,
                        opp.legs.len(),
                        opp.net_edge_cents,
                        opp.reason
                    );
                    write_trade_event(
                        &mut trade_journal,
                        json!({
                            "ts": now_iso(),
                            "kind": "opportunity_found",
                            "fingerprint": fingerprint,
                            "event_ticker": &opp.group_event_ticker,
                            "direction": direction_label(opp.direction),
                            "qty": opp.qty,
                            "gross_edge_cents": opp.gross_edge_cents,
                            "net_edge_cents": opp.net_edge_cents,
                            "reason": &opp.reason,
                            "legs": legs_json(&opp)
                        }),
                    );
                }

                if cli.dry_run {
                    if is_new {
                        info!("Dry-run: skipping execution.");
                        write_trade_event(
                            &mut trade_journal,
                            json!({
                                "ts": now_iso(),
                                "kind": "dry_run_skip",
                                "fingerprint": fingerprint,
                                "event_ticker": &opp.group_event_ticker,
                                "direction": direction_label(opp.direction),
                                "qty": opp.qty,
                                "net_edge_cents": opp.net_edge_cents
                            }),
                        );
                    }
                    continue;
                }

                if last_balance_ok.is_some_and(|t| t.elapsed() > Duration::from_secs(120)) {
                    warn!(
                        "Skipping execution for {}: balance heartbeat stale",
                        opp.group_event_ticker
                    );
                    write_trade_event(
                        &mut trade_journal,
                        json!({
                            "ts": now_iso(),
                            "kind": "execution_skip",
                            "reason": "balance_heartbeat_stale",
                            "fingerprint": fingerprint,
                            "event_ticker": &opp.group_event_ticker
                        }),
                    );
                    continue;
                }

                // Risk check.
                if let Err(e) = risk.check(&opp) {
                    warn!("Risk Rejected: {}", e);
                    risk_rejects_since_heartbeat = risk_rejects_since_heartbeat.saturating_add(1);
                    write_trade_event(
                        &mut trade_journal,
                        json!({
                            "ts": now_iso(),
                            "kind": "risk_rejected",
                            "fingerprint": fingerprint,
                            "event_ticker": &opp.group_event_ticker,
                            "error": e.to_string()
                        }),
                    );
                    continue;
                }

                // Avoid cancellation of in-flight order flows; let the executor
                // finish and reconcile any partial fills.
                let exec_started = Instant::now();
                execution_attempts_since_heartbeat =
                    execution_attempts_since_heartbeat.saturating_add(1);
                write_trade_event(
                    &mut trade_journal,
                    json!({
                        "ts": now_iso(),
                        "kind": "execution_start",
                        "fingerprint": fingerprint,
                        "event_ticker": &opp.group_event_ticker,
                        "direction": direction_label(opp.direction),
                        "qty": opp.qty,
                        "legs": legs_json(&opp),
                        "net_edge_cents": opp.net_edge_cents
                    }),
                );
                let exec_result = exec.execute(&opp).await;
                let exec_elapsed = exec_started.elapsed();
                if exec_elapsed > Duration::from_secs(12) {
                    warn!(
                        "Execution for {} took {}ms",
                        opp.group_event_ticker,
                        exec_elapsed.as_millis()
                    );
                }
                write_trade_event(
                    &mut trade_journal,
                    json!({
                        "ts": now_iso(),
                        "kind": "execution_result",
                        "fingerprint": fingerprint,
                        "event_ticker": &opp.group_event_ticker,
                        "result": outcome_label(exec_result),
                        "elapsed_ms": exec_elapsed.as_millis() as u64,
                        "net_edge_cents": opp.net_edge_cents
                    }),
                );

                // Update risk only when exposure may persist.
                match exec_result {
                    ExecutionOutcome::CompleteFill => {
                        risk.record_execution(&opp);
                        risk.clear_critical_failures();
                    }
                    ExecutionOutcome::PartialFillUnwindFailed => {
                        error!(
                            "Execution for {} had partial fill with unwind failure; recording conservative exposure",
                            opp.group_event_ticker
                        );
                        execution_failures_since_heartbeat =
                            execution_failures_since_heartbeat.saturating_add(1);
                        risk.record_execution(&opp);
                        if let Err(e) = risk.record_critical_failure(&format!(
                            "{} partial-fill unwind failure",
                            opp.group_event_ticker
                        )) {
                            error!("{}", e);
                            write_trade_event(
                                &mut trade_journal,
                                json!({
                                    "ts": now_iso(),
                                    "kind": "risk_kill_switch",
                                    "reason": "partial_fill_unwind_failed",
                                    "event_ticker": &opp.group_event_ticker,
                                    "error": e.to_string()
                                }),
                            );
                            return;
                        }
                    }
                    ExecutionOutcome::PartialFillUnwound => {
                        warn!(
                            "Execution for {} was partially filled but unwind completed",
                            opp.group_event_ticker
                        );
                        risk.clear_critical_failures();
                    }
                    ExecutionOutcome::NoFill => {
                        debug!("Execution for {} had no fill", opp.group_event_ticker);
                        risk.clear_critical_failures();
                    }
                    ExecutionOutcome::Rejected => {
                        warn!(
                            "Execution for {} rejected by pre-trade validation",
                            opp.group_event_ticker
                        );
                        risk.clear_critical_failures();
                    }
                    ExecutionOutcome::Failed => {
                        error!("Execution for {} failed", opp.group_event_ticker);
                        execution_failures_since_heartbeat =
                            execution_failures_since_heartbeat.saturating_add(1);
                        if let Err(e) = risk.record_critical_failure(&format!(
                            "{} execution failed",
                            opp.group_event_ticker
                        )) {
                            error!("{}", e);
                            write_trade_event(
                                &mut trade_journal,
                                json!({
                                    "ts": now_iso(),
                                    "kind": "risk_kill_switch",
                                    "reason": "execution_failed_threshold",
                                    "event_ticker": &opp.group_event_ticker,
                                    "error": e.to_string()
                                }),
                            );
                            return;
                        }
                    }
                }
            }
        }

        if cli.dry_run {
            if new_opp_count == 0 && !current_cycle_opps.is_empty() {
                debug!(
                    "Dry-run: {} opportunities unchanged from previous scan",
                    current_cycle_opps.len()
                );
            } else if current_cycle_opps.is_empty() {
                debug!("Dry-run: no opportunities this scan");
            }
        }

        if last_heartbeat.elapsed() >= HEARTBEAT_INTERVAL {
            let tracked_tickers = tickers_to_track.read().await.len();
            let cached_quotes = quote_book.inner().read().await.len();
            info!(
                "HEARTBEAT: cycles={} groups={} active_opps={} tickers={} quotes={} new_opps={} exec_attempts={} risk_rejects={} exec_failures={}",
                cycles_since_heartbeat,
                group_count,
                current_cycle_opps.len(),
                tracked_tickers,
                cached_quotes,
                new_opps_since_heartbeat,
                execution_attempts_since_heartbeat,
                risk_rejects_since_heartbeat,
                execution_failures_since_heartbeat
            );
            write_trade_event(
                &mut trade_journal,
                json!({
                    "ts": now_iso(),
                    "kind": "heartbeat",
                    "mode": if cli.dry_run { "dry_run" } else { "live" },
                    "cycles": cycles_since_heartbeat,
                    "groups": group_count,
                    "active_opportunities": current_cycle_opps.len(),
                    "tracked_tickers": tracked_tickers,
                    "cached_quotes": cached_quotes,
                    "new_opportunities": new_opps_since_heartbeat,
                    "execution_attempts": execution_attempts_since_heartbeat,
                    "risk_rejections": risk_rejects_since_heartbeat,
                    "execution_failures": execution_failures_since_heartbeat
                }),
            );
            last_heartbeat = Instant::now();
            cycles_since_heartbeat = 0;
            new_opps_since_heartbeat = 0;
            execution_attempts_since_heartbeat = 0;
            risk_rejects_since_heartbeat = 0;
            execution_failures_since_heartbeat = 0;
        }

        prev_cycle_opps = current_cycle_opps;

        sleep(Duration::from_millis(cfg.timing.eval_interval_ms)).await;
    }
}
