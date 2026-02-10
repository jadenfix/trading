//! Arbitrage Bot Entry Point.
//!
//! Orchestrates the tasks:
//! 1. Universe Discovery (periodic)
//! 2. WebSocket stream (continuous)
//! 3. Arb Evaluation Loop (continuous)

mod config;

use std::sync::Arc;
use std::time::Duration;

use arb_strategy::{
    ArbExecutor, ArbEvaluator, ArbFeeModel, ArbRiskManager, QuoteBook, UniverseBuilder,
};
use clap::Parser;
use kalshi_client::{KalshiAuth, KalshiRestClient, KalshiWsClient, new_price_cache};
use tokio::sync::RwLock;
use tokio::time::sleep;
use tracing::{error, info, warn};

use crate::config::load_config;

#[derive(Parser)]
struct Cli {
    #[arg(long)]
    check_auth: bool,

    #[arg(long)]
    dry_run: bool,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("info,arbitrage_bot=debug,arb_strategy=debug")
        .init();

    info!("🚀 Arbitrage Bot starting...");

    let cli = Cli::parse();
    let cfg = match load_config() {
        Ok(c) => c,
        Err(e) => {
            error!("Config error: {}", e);
            return;
        }
    };

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
            Ok(bal) => info!("✅ Auth valid. Balance: {}¢", bal),
            Err(e) => error!("❌ Auth failed: {}", e),
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
            
            info!("Universe updated: {} groups, {} unique tickers", groups.len(), all_tickers.len());
            
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
    let exec = ArbExecutor::new(&rest_client, &fees);
    
    // Wait for initial universe.
    info!("Waiting for universe discovery...");
    while strategy_universe.read().await.is_empty() {
        sleep(Duration::from_secs(2)).await;
    }
    
    info!("Starting evaluation loop...");
    
    loop {
        let groups = strategy_universe.read().await.clone();
        
        let evaluator = ArbEvaluator::new(&fees, &quote_book);
        
        for group in groups {
            // Evaluate.
            if let Some(opp) = evaluator.evaluate(
                &group,
                cfg.arb.min_profit_cents,
                cfg.arb.ev_mode_enabled,
                cfg.arb.tie_buffer_cents,
                cfg.timing.quote_stale_secs,
                cfg.arb.default_qty,
            ).await {
                info!("OPPORTUNITY FOUND: {:?}", opp);
                
                if cli.dry_run {
                    info!("Dry-run: skipping execution.");
                    continue;
                }
                
                // Risk check.
                if let Err(e) = risk.check(&opp) {
                    warn!("Risk Rejected: {}", e);
                    continue;
                }
                
                // Execute.
                exec.execute(&opp).await;
                risk.record_execution(&opp);
            }
        }
        
        sleep(Duration::from_millis(cfg.timing.eval_interval_ms)).await;
    }
}
