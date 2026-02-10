//! Weather-bot: Kalshi weather mispricing bot.
//!
//! Single-binary Tokio application that:
//! 1. Discovers weather markets on Kalshi
//! 2. Streams live prices via WebSocket
//! 3. Fetches NOAA forecasts
//! 4. Evaluates mispricing opportunities
//! 5. Executes orders with risk controls

mod config;

use std::{
    collections::HashMap,
    fs::{create_dir_all, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use chrono::{DateTime, SecondsFormat, Utc};
use clap::Parser;
use serde_json::json;
use tokio::sync::{Mutex, RwLock};
use tokio::time::sleep;
use tracing::{error, info, warn};

use common::config::BotConfig;
use kalshi_client::{new_price_cache, KalshiAuth, KalshiRestClient, KalshiWsClient};
use noaa_client::NoaaClient;
use strategy::{new_forecast_cache, ForecastEntry, RiskManager, StrategyEngine};

/// Kalshi Weather Mispricing Bot
#[derive(Parser)]
#[command(name = "weather-bot", about = "Kalshi weather mispricing bot")]
struct Cli {
    /// Just test authentication and print balance, then exit.
    #[arg(long)]
    check_auth: bool,

    /// Run a single strategy evaluation and exit (dry-run).
    #[arg(long)]
    dry_run: bool,
}

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const BOT_TRADE_DIR: &str = "weather-bot";

type SharedTradeJournal = Arc<Mutex<TradeJournal>>;

fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn resolves_within_days_window(
    market: &common::MarketInfo,
    now: DateTime<Utc>,
    max_days_to_resolution: i64,
) -> bool {
    if max_days_to_resolution <= 0 {
        return false;
    }

    let Some(resolution_time) = market.close_time.or(market.expiration_time) else {
        return false;
    };

    let remaining = resolution_time - now;
    remaining > chrono::Duration::zero()
        && remaining < chrono::Duration::days(max_days_to_resolution)
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
            return PathBuf::from(trimmed).join(BOT_TRADE_DIR);
        }
    }

    if let Some(root) = resolve_repo_root() {
        return root.join("TRADES").join(BOT_TRADE_DIR);
    }

    PathBuf::from("TRADES").join(BOT_TRADE_DIR)
}

fn action_label(action: common::Action) -> &'static str {
    match action {
        common::Action::Buy => "buy",
        common::Action::Sell => "sell",
    }
}

fn side_label(side: common::Side) -> &'static str {
    match side {
        common::Side::Yes => "yes",
        common::Side::No => "no",
    }
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

async fn write_trade_event(journal: &SharedTradeJournal, event: serde_json::Value) {
    let mut guard = journal.lock().await;
    guard.write_event(event);
}

#[tokio::main]
async fn main() {
    // Initialize logging.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "weather_bot=info,kalshi_client=info,noaa_client=info,strategy=info".into()
            }),
        )
        .with_target(true)
        .init();

    let cli = Cli::parse();

    info!("🌤️  Weather Bot starting up...");

    // Load configuration.
    let cfg = match config::load_config() {
        Ok(c) => c,
        Err(e) => {
            error!("Configuration error: {}", e);
            std::process::exit(1);
        }
    };

    let env_label = if cfg.use_demo { "DEMO" } else { "PRODUCTION" };
    info!("Environment: {}", env_label);
    info!(
        "Cities: {:?}",
        cfg.cities.iter().map(|c| &c.name).collect::<Vec<_>>()
    );
    info!(
        "Strategy: entry≤{}¢, exit≥{}¢, edge≥{}¢, max_pos={}¢, window<{}d",
        cfg.strategy.entry_threshold_cents,
        cfg.strategy.exit_threshold_cents,
        cfg.strategy.edge_threshold_cents,
        cfg.strategy.max_position_cents,
        cfg.strategy.max_days_to_resolution,
    );
    info!(
        "Risk: max_city={}¢, max_total={}¢, max_daily_loss={}¢, orders/min={}",
        cfg.risk.max_city_exposure_cents,
        cfg.risk.max_total_exposure_cents,
        cfg.risk.max_daily_loss_cents,
        cfg.risk.max_orders_per_minute,
    );
    info!(
        "Fees: taker={:.2}%, maker={:.4}%",
        0.07 * 100.0,
        0.0175 * 100.0
    );

    let trades_dir = resolve_trades_dir();
    let journal = match TradeJournal::open(trades_dir) {
        Ok(j) => j,
        Err(e) => {
            error!("Failed to initialize trade journal: {}", e);
            std::process::exit(1);
        }
    };
    let journal_path = journal.dir().to_path_buf();
    let trade_journal: SharedTradeJournal = Arc::new(Mutex::new(journal));
    info!("Trade journal path: {}", journal_path.display());
    write_trade_event(
        &trade_journal,
        json!({
            "ts": now_iso(),
            "kind": "bot_start",
            "bot": "weather-bot",
            "mode": if cli.dry_run { "dry_run" } else { "live" },
            "use_demo": cfg.use_demo,
            "cities": cfg.cities.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
            "strategy": {
                "entry_threshold_cents": cfg.strategy.entry_threshold_cents,
                "exit_threshold_cents": cfg.strategy.exit_threshold_cents,
                "edge_threshold_cents": cfg.strategy.edge_threshold_cents,
                "max_trades_per_run": cfg.strategy.max_trades_per_run,
                "max_spread_cents": cfg.strategy.max_spread_cents,
                "max_position_cents": cfg.strategy.max_position_cents,
                "max_days_to_resolution": cfg.strategy.max_days_to_resolution
            },
            "timing": {
                "scan_interval_secs": cfg.timing.scan_interval_secs,
                "forecast_interval_secs": cfg.timing.forecast_interval_secs,
                "discovery_interval_secs": cfg.timing.discovery_interval_secs
            }
        }),
    )
    .await;

    // Initialize auth.
    let auth = match KalshiAuth::new(&cfg.api_key, &cfg.secret_key) {
        Ok(a) => a,
        Err(e) => {
            error!("Auth initialization failed: {}", e);
            write_trade_event(
                &trade_journal,
                json!({
                    "ts": now_iso(),
                    "kind": "auth_init",
                    "status": "error",
                    "error": e.to_string()
                }),
            )
            .await;
            std::process::exit(1);
        }
    };

    let rest_client = KalshiRestClient::new(auth.clone(), cfg.use_demo);

    // ── Check-auth mode ──────────────────────────────────────────────
    if cli.check_auth {
        info!("Running auth check...");
        match rest_client.get_balance().await {
            Ok(balance) => {
                info!(
                    "✅ Auth successful! Balance: {}¢ (${:.2})",
                    balance,
                    balance as f64 / 100.0
                );
                write_trade_event(
                    &trade_journal,
                    json!({
                        "ts": now_iso(),
                        "kind": "auth_check",
                        "status": "ok",
                        "balance_cents": balance
                    }),
                )
                .await;
            }
            Err(e) => {
                error!("❌ Auth check failed: {}", e);
                write_trade_event(
                    &trade_journal,
                    json!({
                        "ts": now_iso(),
                        "kind": "auth_check",
                        "status": "error",
                        "error": e.to_string()
                    }),
                )
                .await;
                std::process::exit(1);
            }
        }
        return;
    }

    // ── Shared state ─────────────────────────────────────────────────
    let price_cache = new_price_cache();
    let forecast_cache = new_forecast_cache();
    let tracked_tickers: Arc<RwLock<Vec<String>>> = Arc::new(RwLock::new(Vec::new()));
    let tracked_markets: Arc<RwLock<HashMap<String, common::MarketInfo>>> =
        Arc::new(RwLock::new(HashMap::new()));

    let noaa = NoaaClient::new();
    let strategy = StrategyEngine::new(cfg.clone());
    let _risk_mgr = RiskManager::new(cfg.risk.clone());

    // ── Dry-run mode ─────────────────────────────────────────────────
    if cli.dry_run {
        info!("Running single dry-run evaluation...");
        run_discovery(&rest_client, &cfg, &tracked_tickers, &tracked_markets).await;
        run_forecast_update(&noaa, &cfg, &forecast_cache).await;

        let tickers = tracked_tickers.read().await.clone();
        info!("Discovered {} tickers", tickers.len());

        let markets = tracked_markets.read().await.clone();
        let prices = price_cache.read().await.clone();
        let positions = HashMap::new();

        let intents = strategy.evaluate(&markets, &prices, &forecast_cache, &positions);
        info!(
            "Strategy produced {} intents (dry-run, not executing)",
            intents.len()
        );
        write_trade_event(
            &trade_journal,
            json!({
                "ts": now_iso(),
                "kind": "dry_run_summary",
                "tickers": tickers.len(),
                "markets": markets.len(),
                "prices": prices.len(),
                "forecasts": forecast_cache.len(),
                "intents": intents.len()
            }),
        )
        .await;
        for intent in &intents {
            info!(
                "  → {} {} {} @ {}¢ x{} (fee≈{}¢, conf={:.2}) — {}",
                match intent.action {
                    common::Action::Buy => "BUY",
                    common::Action::Sell => "SELL",
                },
                match intent.side {
                    common::Side::Yes => "YES",
                    common::Side::No => "NO",
                },
                intent.ticker,
                intent.price_cents,
                intent.count,
                intent.estimated_fee_cents,
                intent.confidence,
                intent.reason,
            );
            write_trade_event(
                &trade_journal,
                json!({
                    "ts": now_iso(),
                    "kind": "dry_run_intent",
                    "ticker": &intent.ticker,
                    "action": action_label(intent.action),
                    "side": side_label(intent.side),
                    "price_cents": intent.price_cents,
                    "count": intent.count,
                    "estimated_fee_cents": intent.estimated_fee_cents,
                    "confidence": intent.confidence,
                    "reason": &intent.reason
                }),
            )
            .await;
        }
        return;
    }

    // ── Spawn tasks ──────────────────────────────────────────────────
    info!("Spawning tasks...");

    // Task 1: Market Discovery
    let disc_client = rest_client.clone();
    let disc_cfg = cfg.clone();
    let disc_tickers = tracked_tickers.clone();
    let disc_markets = tracked_markets.clone();
    let disc_journal = trade_journal.clone();
    let discovery_handle = tokio::spawn(async move {
        loop {
            run_discovery(&disc_client, &disc_cfg, &disc_tickers, &disc_markets).await;
            let tracked_count = disc_markets.read().await.len();
            write_trade_event(
                &disc_journal,
                json!({
                    "ts": now_iso(),
                    "kind": "discovery_cycle",
                    "markets": tracked_count,
                    "series_prefixes": disc_cfg.series_prefixes.len()
                }),
            )
            .await;
            sleep(Duration::from_secs(disc_cfg.timing.discovery_interval_secs)).await;
        }
    });

    // Task 2: WebSocket Price Feed
    let ws_client = KalshiWsClient::new(auth.clone(), cfg.use_demo, price_cache.clone());
    let ws_tickers = tracked_tickers.clone();
    let ws_handle = tokio::spawn(async move {
        ws_client.run(ws_tickers).await;
    });

    // Task 3: Forecast Updates
    let fc_noaa = noaa.clone();
    let fc_cfg = cfg.clone();
    let fc_cache = forecast_cache.clone();
    let fc_journal = trade_journal.clone();
    let forecast_handle = tokio::spawn(async move {
        loop {
            run_forecast_update(&fc_noaa, &fc_cfg, &fc_cache).await;
            write_trade_event(
                &fc_journal,
                json!({
                    "ts": now_iso(),
                    "kind": "forecast_cycle",
                    "cities": fc_cfg.cities.len(),
                    "cached_forecasts": fc_cache.len()
                }),
            )
            .await;
            sleep(Duration::from_secs(fc_cfg.timing.forecast_interval_secs)).await;
        }
    });

    // Task 4: Strategy Loop
    let strat_client = rest_client.clone();
    let strat_cfg = cfg.clone();
    let strat_price_cache = price_cache.clone();
    let strat_forecast_cache = forecast_cache.clone();
    let strat_markets = tracked_markets.clone();
    let strat_journal = trade_journal.clone();
    let strategy_handle = tokio::spawn(async move {
        // Wait a bit for initial data to arrive.
        sleep(Duration::from_secs(15)).await;

        let strategy = StrategyEngine::new(strat_cfg.clone());
        let mut risk_mgr = RiskManager::new(strat_cfg.risk.clone());
        let mut cycle_id: u64 = 0;

        loop {
            cycle_id = cycle_id.saturating_add(1);
            run_strategy_cycle(
                &strat_client,
                &strategy,
                &mut risk_mgr,
                &strat_price_cache,
                &strat_forecast_cache,
                &strat_markets,
                &strat_journal,
                cycle_id,
            )
            .await;
            sleep(Duration::from_secs(strat_cfg.timing.scan_interval_secs)).await;
        }
    });

    // Task 5: Heartbeat
    let hb_cfg = cfg.clone();
    let hb_tickers = tracked_tickers.clone();
    let hb_markets = tracked_markets.clone();
    let hb_prices = price_cache.clone();
    let hb_forecasts = forecast_cache.clone();
    let hb_journal = trade_journal.clone();
    let heartbeat_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(HEARTBEAT_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            interval.tick().await;
            let tickers = hb_tickers.read().await.len();
            let markets = hb_markets.read().await.len();
            let prices = hb_prices.read().await.len();
            let forecasts = hb_forecasts.len();
            info!(
                "HEARTBEAT: tickers={} markets={} prices={} forecasts={} scan={}s",
                tickers, markets, prices, forecasts, hb_cfg.timing.scan_interval_secs
            );
            write_trade_event(
                &hb_journal,
                json!({
                    "ts": now_iso(),
                    "kind": "heartbeat",
                    "tickers": tickers,
                    "markets": markets,
                    "prices": prices,
                    "forecasts": forecasts,
                    "scan_interval_secs": hb_cfg.timing.scan_interval_secs
                }),
            )
            .await;
        }
    });

    // ── Wait for shutdown ────────────────────────────────────────────
    info!("🚀 Weather Bot is running. Press Ctrl+C to stop.");

    let shutdown_reason = tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("Shutdown signal received");
            "ctrl_c"
        }
        r = discovery_handle => {
            error!("Discovery task exited: {:?}", r);
            "discovery_task_exit"
        }
        r = ws_handle => {
            error!("WebSocket task exited: {:?}", r);
            "ws_task_exit"
        }
        r = forecast_handle => {
            error!("Forecast task exited: {:?}", r);
            "forecast_task_exit"
        }
        r = strategy_handle => {
            error!("Strategy task exited: {:?}", r);
            "strategy_task_exit"
        }
        r = heartbeat_handle => {
            error!("Heartbeat task exited: {:?}", r);
            "heartbeat_task_exit"
        }
    };

    write_trade_event(
        &trade_journal,
        json!({
            "ts": now_iso(),
            "kind": "bot_shutdown",
            "reason": shutdown_reason
        }),
    )
    .await;

    info!("Weather Bot shut down.");
}

// ── Task implementations ────────────────────────────────────────────

async fn run_discovery(
    client: &KalshiRestClient,
    cfg: &BotConfig,
    tickers: &Arc<RwLock<Vec<String>>>,
    markets: &Arc<RwLock<HashMap<String, common::MarketInfo>>>,
) {
    info!("Running market discovery...");
    let mut all_markets = Vec::new();

    for prefix in &cfg.series_prefixes {
        match client.get_markets(Some(prefix), Some("open"), 200).await {
            Ok(mkts) => {
                info!("Found {} markets for series {}", mkts.len(), prefix);
                all_markets.extend(mkts);
            }
            Err(e) => {
                warn!("Failed to fetch markets for {}: {}", prefix, e);
            }
        }
    }

    // Also try without series_ticker to catch any weather markets.
    // Filter by checking if ticker contains known city prefixes.
    if all_markets.is_empty() {
        info!("No series-specific markets found; trying broad search...");
        match client.get_markets(None, Some("open"), 100).await {
            Ok(mkts) => {
                for mkt in mkts {
                    let t = mkt.ticker.to_uppercase();
                    for prefix in &cfg.series_prefixes {
                        if t.contains(&prefix.to_uppercase()) {
                            all_markets.push(mkt.clone());
                            break;
                        }
                    }
                }
                info!("Broad search found {} matching markets", all_markets.len());
            }
            Err(e) => {
                warn!("Broad market search failed: {}", e);
            }
        }
    }

    let pre_window_count = all_markets.len();
    let now = Utc::now();
    all_markets
        .retain(|m| resolves_within_days_window(m, now, cfg.strategy.max_days_to_resolution));
    info!(
        "Discovery window: kept {} / {} markets with resolution < {} days",
        all_markets.len(),
        pre_window_count,
        cfg.strategy.max_days_to_resolution
    );

    // Update shared state.
    let new_tickers: Vec<String> = all_markets.iter().map(|m| m.ticker.clone()).collect();
    let new_map: HashMap<String, common::MarketInfo> = all_markets
        .into_iter()
        .map(|m| (m.ticker.clone(), m))
        .collect();

    info!("Tracking {} markets", new_tickers.len());

    *tickers.write().await = new_tickers;
    *markets.write().await = new_map;
}

async fn run_forecast_update(noaa: &NoaaClient, cfg: &BotConfig, cache: &strategy::ForecastCache) {
    info!("Updating forecasts...");

    for city in &cfg.cities {
        match noaa.get_forecast(city).await {
            Ok(forecast) => {
                info!(
                    "Forecast for {}: high={:.0}°F, low={:.0}°F, precip={:.0}%",
                    city.name,
                    forecast.high_temp_f,
                    forecast.low_temp_f,
                    forecast.precip_prob * 100.0
                );
                cache.insert(
                    city.name.clone(),
                    ForecastEntry {
                        data: forecast,
                        updated_at: std::time::Instant::now(),
                    },
                );
            }
            Err(e) => {
                warn!("Failed to fetch forecast for {}: {}", city.name, e);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_strategy_cycle(
    client: &KalshiRestClient,
    strategy: &StrategyEngine,
    risk_mgr: &mut RiskManager,
    price_cache: &kalshi_client::PriceCache,
    forecast_cache: &strategy::ForecastCache,
    tracked_markets: &Arc<RwLock<HashMap<String, common::MarketInfo>>>,
    trade_journal: &SharedTradeJournal,
    cycle_id: u64,
) {
    info!("Running strategy evaluation...");

    // Get current positions and balance.
    let (positions_vec, balance) = match (client.get_positions().await, client.get_balance().await)
    {
        (Ok(p), Ok(b)) => (p, b),
        (Err(e), _) | (_, Err(e)) => {
            warn!("Failed to fetch portfolio state: {}", e);
            write_trade_event(
                trade_journal,
                json!({
                    "ts": now_iso(),
                    "kind": "strategy_cycle_error",
                    "cycle_id": cycle_id,
                    "error": e.to_string()
                }),
            )
            .await;
            return;
        }
    };

    // Set session balance for drawdown tracking.
    risk_mgr.set_session_balance(balance);

    let position_map: HashMap<String, i64> = positions_vec
        .iter()
        .map(|p| (p.ticker.clone(), p.yes_count))
        .collect();

    let markets = tracked_markets.read().await.clone();
    let prices = price_cache.read().await.clone();

    info!(
        "Evaluating: {} markets, {} prices, balance={}¢",
        markets.len(),
        prices.len(),
        balance
    );
    write_trade_event(
        trade_journal,
        json!({
            "ts": now_iso(),
            "kind": "strategy_cycle_start",
            "cycle_id": cycle_id,
            "markets": markets.len(),
            "prices": prices.len(),
            "forecasts": forecast_cache.len(),
            "positions": positions_vec.len(),
            "balance_cents": balance
        }),
    )
    .await;

    // Run strategy.
    let intents = strategy.evaluate(&markets, &prices, forecast_cache, &position_map);
    let generated_count = intents.len();
    if generated_count > 0 {
        for intent in &intents {
            write_trade_event(
                trade_journal,
                json!({
                    "ts": now_iso(),
                    "kind": "intent_generated",
                    "cycle_id": cycle_id,
                    "ticker": &intent.ticker,
                    "action": action_label(intent.action),
                    "side": side_label(intent.side),
                    "price_cents": intent.price_cents,
                    "count": intent.count,
                    "estimated_fee_cents": intent.estimated_fee_cents,
                    "confidence": intent.confidence,
                    "reason": &intent.reason
                }),
            )
            .await;
        }
    }

    if intents.is_empty() {
        info!("No trading opportunities found this cycle");
        write_trade_event(
            trade_journal,
            json!({
                "ts": now_iso(),
                "kind": "strategy_cycle_summary",
                "cycle_id": cycle_id,
                "generated": 0,
                "approved": 0,
                "risk_rejected": 0,
                "orders_placed": 0,
                "orders_failed": 0,
                "balance_cents": balance
            }),
        )
        .await;
        return;
    }

    // Filter through risk manager.
    let approved = risk_mgr.filter_intents(intents, &positions_vec, balance);
    let approved_count = approved.len();
    let risk_rejected_count = generated_count.saturating_sub(approved_count);

    if approved.is_empty() {
        info!("All intents rejected by risk manager");
        write_trade_event(
            trade_journal,
            json!({
                "ts": now_iso(),
                "kind": "strategy_cycle_summary",
                "cycle_id": cycle_id,
                "generated": generated_count,
                "approved": 0,
                "risk_rejected": risk_rejected_count,
                "orders_placed": 0,
                "orders_failed": 0,
                "balance_cents": balance
            }),
        )
        .await;
        return;
    }

    // Execute approved orders.
    let mut placed = 0usize;
    let mut failed = 0usize;
    for intent in &approved {
        match client.create_order(intent).await {
            Ok(resp) => {
                risk_mgr.record_order();
                placed = placed.saturating_add(1);
                info!(
                    "✅ Order placed: {} {} {} — id={}, status={}, filled={}, fee≈{}¢",
                    match intent.action {
                        common::Action::Buy => "BUY",
                        common::Action::Sell => "SELL",
                    },
                    match intent.side {
                        common::Side::Yes => "YES",
                        common::Side::No => "NO",
                    },
                    intent.ticker,
                    resp.order.order_id,
                    resp.order.status,
                    resp.order.fill_count,
                    intent.estimated_fee_cents,
                );
                write_trade_event(
                    trade_journal,
                    json!({
                        "ts": now_iso(),
                        "kind": "order_placed",
                        "cycle_id": cycle_id,
                        "ticker": &intent.ticker,
                        "action": action_label(intent.action),
                        "side": side_label(intent.side),
                        "price_cents": intent.price_cents,
                        "count": intent.count,
                        "estimated_fee_cents": intent.estimated_fee_cents,
                        "order_id": resp.order.order_id,
                        "status": resp.order.status,
                        "fill_count": resp.order.fill_count
                    }),
                )
                .await;
            }
            Err(e) => {
                error!("❌ Order failed for {}: {}", intent.ticker, e);
                failed = failed.saturating_add(1);
                write_trade_event(
                    trade_journal,
                    json!({
                        "ts": now_iso(),
                        "kind": "order_failed",
                        "cycle_id": cycle_id,
                        "ticker": &intent.ticker,
                        "action": action_label(intent.action),
                        "side": side_label(intent.side),
                        "price_cents": intent.price_cents,
                        "count": intent.count,
                        "error": e.to_string()
                    }),
                )
                .await;
            }
        }
    }

    write_trade_event(
        trade_journal,
        json!({
            "ts": now_iso(),
            "kind": "strategy_cycle_summary",
            "cycle_id": cycle_id,
            "generated": generated_count,
            "approved": approved_count,
            "risk_rejected": risk_rejected_count,
            "orders_placed": placed,
            "orders_failed": failed,
            "balance_cents": balance
        }),
    )
    .await;
}
