//! Weather-bot: Kalshi weather mispricing bot.
//!
//! Single-binary Tokio application that:
//! 1. Discovers weather markets on Kalshi
//! 2. Streams live prices via WebSocket
//! 3. Fetches NOAA forecasts
//! 4. Evaluates mispricing opportunities
//! 5. Executes orders with risk controls

mod config;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use tokio::sync::RwLock;
use tokio::time::sleep;
use tracing::{error, info, warn};

use common::config::BotConfig;
use kalshi_client::{KalshiAuth, KalshiRestClient, KalshiWsClient, new_price_cache};
use noaa_client::NoaaClient;
use strategy::{ForecastEntry, StrategyEngine, RiskManager, new_forecast_cache};

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

#[tokio::main]
async fn main() {
    // Initialize logging.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "weather_bot=info,kalshi_client=info,noaa_client=info,strategy=info".into()),
        )
        .with_target(true)
        .with_timer(tracing_subscriber::fmt::time::UtcTime::rfc_3339())
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
    info!("Cities: {:?}", cfg.cities.iter().map(|c| &c.name).collect::<Vec<_>>());
    info!(
        "Strategy: entry≤{}¢, exit≥{}¢, edge≥{}¢, max_pos={}¢",
        cfg.strategy.entry_threshold_cents,
        cfg.strategy.exit_threshold_cents,
        cfg.strategy.edge_threshold_cents,
        cfg.strategy.max_position_cents,
    );

    // Initialize auth.
    let auth = match KalshiAuth::new(&cfg.api_key, &cfg.secret_key) {
        Ok(a) => a,
        Err(e) => {
            error!("Auth initialization failed: {}", e);
            std::process::exit(1);
        }
    };

    let rest_client = KalshiRestClient::new(auth.clone(), cfg.use_demo);

    // ── Check-auth mode ──────────────────────────────────────────────
    if cli.check_auth {
        info!("Running auth check...");
        match rest_client.get_balance().await {
            Ok(balance) => {
                info!("✅ Auth successful! Balance: {}¢ (${:.2})", balance, balance as f64 / 100.0);
            }
            Err(e) => {
                error!("❌ Auth check failed: {}", e);
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
    let risk_mgr = RiskManager::new(cfg.strategy.max_position_cents);

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
        info!("Strategy produced {} intents (dry-run, not executing)", intents.len());
        for intent in &intents {
            info!("  → {:?}", intent);
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
    let discovery_handle = tokio::spawn(async move {
        loop {
            run_discovery(&disc_client, &disc_cfg, &disc_tickers, &disc_markets).await;
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
    let forecast_handle = tokio::spawn(async move {
        loop {
            run_forecast_update(&fc_noaa, &fc_cfg, &fc_cache).await;
            sleep(Duration::from_secs(fc_cfg.timing.forecast_interval_secs)).await;
        }
    });

    // Task 4: Strategy Loop
    let strat_client = rest_client.clone();
    let strat_cfg = cfg.clone();
    let strat_price_cache = price_cache.clone();
    let strat_forecast_cache = forecast_cache.clone();
    let strat_markets = tracked_markets.clone();
    let strategy_handle = tokio::spawn(async move {
        // Wait a bit for initial data to arrive.
        sleep(Duration::from_secs(15)).await;

        let strategy = StrategyEngine::new(strat_cfg.clone());
        let risk_mgr = RiskManager::new(strat_cfg.strategy.max_position_cents);

        loop {
            run_strategy_cycle(
                &strat_client,
                &strategy,
                &risk_mgr,
                &strat_price_cache,
                &strat_forecast_cache,
                &strat_markets,
            )
            .await;
            sleep(Duration::from_secs(strat_cfg.timing.scan_interval_secs)).await;
        }
    });

    // ── Wait for shutdown ────────────────────────────────────────────
    info!("🚀 Weather Bot is running. Press Ctrl+C to stop.");

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("Shutdown signal received");
        }
        r = discovery_handle => {
            error!("Discovery task exited: {:?}", r);
        }
        r = ws_handle => {
            error!("WebSocket task exited: {:?}", r);
        }
        r = forecast_handle => {
            error!("Forecast task exited: {:?}", r);
        }
        r = strategy_handle => {
            error!("Strategy task exited: {:?}", r);
        }
    }

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

async fn run_forecast_update(
    noaa: &NoaaClient,
    cfg: &BotConfig,
    cache: &strategy::ForecastCache,
) {
    info!("Updating forecasts...");

    for city in &cfg.cities {
        match noaa.get_forecast(city).await {
            Ok(forecast) => {
                info!(
                    "Forecast for {}: high={:.0}°F, low={:.0}°F, precip={:.0}%",
                    city.name, forecast.high_temp_f, forecast.low_temp_f,
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

async fn run_strategy_cycle(
    client: &KalshiRestClient,
    strategy: &StrategyEngine,
    risk_mgr: &RiskManager,
    price_cache: &kalshi_client::PriceCache,
    forecast_cache: &strategy::ForecastCache,
    tracked_markets: &Arc<RwLock<HashMap<String, common::MarketInfo>>>,
) {
    info!("Running strategy evaluation...");

    // Get current positions and balance.
    let (positions_vec, balance) = match (client.get_positions().await, client.get_balance().await) {
        (Ok(p), Ok(b)) => (p, b),
        (Err(e), _) | (_, Err(e)) => {
            warn!("Failed to fetch portfolio state: {}", e);
            return;
        }
    };

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

    // Run strategy.
    let intents = strategy.evaluate(&markets, &prices, forecast_cache, &position_map);

    if intents.is_empty() {
        info!("No trading opportunities found this cycle");
        return;
    }

    // Filter through risk manager.
    let approved = risk_mgr.filter_intents(intents, &positions_vec, balance);

    if approved.is_empty() {
        info!("All intents rejected by risk manager");
        return;
    }

    // Execute approved orders.
    for intent in &approved {
        match client.create_order(intent).await {
            Ok(resp) => {
                info!(
                    "✅ Order placed: {} {} {} — id={}, status={}, filled={}",
                    match intent.action { common::Action::Buy => "BUY", common::Action::Sell => "SELL" },
                    match intent.side { common::Side::Yes => "YES", common::Side::No => "NO" },
                    intent.ticker,
                    resp.order.order_id,
                    resp.order.status,
                    resp.order.fill_count,
                );
            }
            Err(e) => {
                error!("❌ Order failed for {}: {}", intent.ticker, e);
            }
        }
    }
}
