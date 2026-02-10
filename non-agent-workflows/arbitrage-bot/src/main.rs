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
    hash::{Hash, Hasher},
};

use arb_strategy::{
    ArbDirection, ArbEvaluator, ArbExecutor, ArbFeeModel, ArbOpportunity, ArbRiskManager,
    ExecutionOutcome, QuoteBook, UniverseBuilder,
};
use clap::Parser;
use kalshi_client::{new_price_cache, KalshiAuth, KalshiRestClient, KalshiWsClient, PriceEntry};
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
    if cfg.timing.quote_stale_secs < 10 {
        warn!(
            "quote_stale_secs={} is very low; many markets may be skipped as stale",
            cfg.timing.quote_stale_secs
        );
    }

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
    let exec = ArbExecutor::new(&rest_client, &fees);

    // Wait for initial universe.
    info!("Waiting for universe discovery...");
    while strategy_universe.read().await.is_empty() {
        sleep(Duration::from_secs(2)).await;
    }

    info!("Starting evaluation loop...");
    let mut prev_cycle_opps: HashSet<u64> = HashSet::new();

    loop {
        let groups = strategy_universe.read().await.clone();
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
                    cfg.arb.tie_buffer_cents,
                    cfg.timing.quote_stale_secs,
                    cfg.arb.default_qty,
                )
                .await
            {
                let fingerprint = opportunity_fingerprint(&opp);
                current_cycle_opps.insert(fingerprint);

                if !prev_cycle_opps.contains(&fingerprint) {
                    new_opp_count += 1;
                    info!(
                        "OPPORTUNITY FOUND: event={} direction={:?} legs={} net={}¢ reason={}",
                        opp.group_event_ticker,
                        opp.direction,
                        opp.legs.len(),
                        opp.net_edge_cents,
                        opp.reason
                    );
                }

                if cli.dry_run {
                    if !prev_cycle_opps.contains(&fingerprint) {
                        info!("Dry-run: skipping execution.");
                    }
                    continue;
                }

                // Risk check.
                if let Err(e) = risk.check(&opp) {
                    warn!("Risk Rejected: {}", e);
                    continue;
                }

                // Execute and update risk only when exposure may persist.
                match exec.execute(&opp).await {
                    ExecutionOutcome::CompleteFill => {
                        risk.record_execution(&opp);
                    }
                    ExecutionOutcome::PartialFillUnwindFailed => {
                        error!(
                            "Execution for {} had partial fill with unwind failure; recording conservative exposure",
                            opp.group_event_ticker
                        );
                        risk.record_execution(&opp);
                    }
                    ExecutionOutcome::PartialFillUnwound => {
                        warn!(
                            "Execution for {} was partially filled but unwind completed",
                            opp.group_event_ticker
                        );
                    }
                    ExecutionOutcome::NoFill => {
                        debug!("Execution for {} had no fill", opp.group_event_ticker);
                    }
                    ExecutionOutcome::Rejected => {
                        warn!(
                            "Execution for {} rejected by pre-trade validation",
                            opp.group_event_ticker
                        );
                    }
                    ExecutionOutcome::Failed => {
                        error!("Execution for {} failed", opp.group_event_ticker);
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

        prev_cycle_opps = current_cycle_opps;

        sleep(Duration::from_millis(cfg.timing.eval_interval_ms)).await;
    }
}
