use std::time::Duration;

use anyhow::{bail, Result};
use candidate_engine::Scanner;
use decision_engine::{Decision, DecisionEngine, DeterministicInput};
use execution_engine::{ExecutionEngine, ExecutionOutcome};
use kalshi_client::{KalshiAuth, KalshiRestClient};
use llm_client::{LlmClient, ResearchError, ResearchRequest, ResearchResponse, RiskLevel};
use serde_json::json;
use tokio::time::sleep;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::budget::BudgetManager;
use crate::config::AppConfig;
use crate::journal::{now_iso, resolve_trades_dir, TradeJournal};
use crate::risk::RiskGuard;
use crate::temporal::TemporalResearchClient;

enum ResearchBackend {
    DirectLlm(LlmClient),
    Temporal(TemporalResearchClient),
}

pub struct Bot {
    config: AppConfig,
    client: KalshiRestClient,
    scanner: Scanner,
    research_backend: ResearchBackend,
    decision_engine: DecisionEngine,
    execution_engine: ExecutionEngine,
    risk_guard: RiskGuard,
    budget_manager: BudgetManager,
    trade_journal: TradeJournal,
}

impl Bot {
    pub async fn new(config: AppConfig) -> Result<Self> {
        if !config.kalshi.api_base.is_empty() {
            std::env::set_var("KALSHI_API_BASE_URL", &config.kalshi.api_base);
        }

        if !config.trading.shadow_mode && !config.trading.live_enable {
            bail!("Invalid mode: shadow_mode=false requires live_enable=true");
        }

        let auth = KalshiAuth::from_env()?;
        let rest_client = KalshiRestClient::new(auth, config.kalshi.use_demo);

        let research_backend = if config.temporal.enabled {
            info!(
                "Research backend: temporal broker at {}",
                config.temporal.broker_base_url
            );
            ResearchBackend::Temporal(TemporalResearchClient::new(&config.temporal)?)
        } else {
            if !config.llm.provider.eq_ignore_ascii_case("anthropic") {
                warn!(
                    "Configured provider '{}' but this workflow currently supports Anthropic only",
                    config.llm.provider
                );
            }

            let anthropic_key =
                std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY must be set");
            ResearchBackend::DirectLlm(LlmClient::new(
                anthropic_key,
                config.llm.model.clone(),
                config.llm.timeout_ms,
                config.llm.max_retries,
            ))
        };

        let risk_veto_level = parse_risk_level(&config.research.rules_risk_veto_level)?;

        let scanner = Scanner::new(rest_client.clone());
        let decision_engine = DecisionEngine::new(
            config.research.min_edge_for_research_cents,
            config.trading.max_position_contracts,
            config.trading.live_min_size_contracts,
            config.research.kelly_fraction,
            config.research.max_uncertainty,
            risk_veto_level,
        );
        let execution_engine = ExecutionEngine::new(
            rest_client.clone(),
            config.trading.shadow_mode,
            config.trading.live_enable,
        );

        let trades_dir = resolve_trades_dir();
        let mut trade_journal = TradeJournal::open(trades_dir.clone())?;
        trade_journal.write_event(json!({
            "ts": now_iso(),
            "kind": "bot_start",
            "mode": if config.trading.shadow_mode { "shadow" } else { "live" },
            "use_demo": config.kalshi.use_demo,
            "research_backend": if config.temporal.enabled { "temporal" } else { "direct_llm" },
            "staleness_budget_ms": config.research.staleness_budget_ms,
            "llm_timeout_ms": config.llm.timeout_ms
        }));
        info!("Trade journal path: {}", trade_journal.dir().display());

        let budget_manager = BudgetManager::load(config.llm_budget.clone(), &trades_dir)?;
        let risk_guard = RiskGuard::new(config.risk.clone());

        Ok(Self {
            config,
            client: rest_client,
            scanner,
            research_backend,
            decision_engine,
            execution_engine,
            risk_guard,
            budget_manager,
            trade_journal,
        })
    }

    pub async fn run(&mut self) -> Result<()> {
        info!("Bot running...");
        loop {
            if let Err(e) = self.run_cycle().await {
                error!("Cycle failed: {:?}", e);
            }
            sleep(Duration::from_millis(self.config.trading.loop_interval_ms)).await;
        }
    }

    fn write_event(&mut self, event: serde_json::Value) {
        self.trade_journal.write_event(event);
    }

    fn research_error_code(err: &ResearchError) -> &'static str {
        match err {
            ResearchError::Timeout => "RESEARCH_TIMEOUT",
            ResearchError::SchemaValidationFailed(_) => "RESEARCH_SCHEMA_REJECT",
            ResearchError::HttpStatus { .. } => "RESEARCH_HTTP_ERROR",
            ResearchError::ApiError(_) => "RESEARCH_API_ERROR",
            ResearchError::JsonError(_) => "RESEARCH_JSON_ERROR",
        }
    }

    async fn request_research(
        &self,
        request: ResearchRequest,
    ) -> Result<(Option<String>, ResearchResponse), ResearchError> {
        match &self.research_backend {
            ResearchBackend::DirectLlm(client) => {
                let response = client.research(request).await?;
                Ok((None, response))
            }
            ResearchBackend::Temporal(client) => {
                let (workflow_id, response) = client.research(request).await?;
                Ok((Some(workflow_id), response))
            }
        }
    }

    async fn run_cycle(&mut self) -> Result<()> {
        let mut positions = self.client.get_positions().await?;
        let mut balance_cents = self.client.get_balance().await?;

        let candidates = self.scanner.scan_markets(self.config.research.max_days_to_expiry).await?;
        info!("Found {} candidates (within {} days)", candidates.len(), self.config.research.max_days_to_expiry);
        self.write_event(json!({
            "ts": now_iso(),
            "kind": "strategy_cycle_start",
            "candidates": candidates.len(),
            "balance_cents": balance_cents,
            "positions": positions.len(),
        }));

        let mut approved = 0usize;
        let mut vetoed = 0usize;
        let mut llm_calls = 0usize;
        let mut llm_failures = 0usize;

        for candidate in candidates {
            if candidate.complexity_score < self.config.research.complexity_threshold {
                continue;
            }

            if candidate.deterministic.best_edge_cents
                < self.config.research.min_edge_for_research_cents as f64
            {
                continue;
            }

            if candidate.market.open_interest < self.config.research.market_liquidity_min_depth
                && candidate.market.volume_24h < self.config.research.market_liquidity_min_depth
            {
                continue;
            }

            let p_det_target = match candidate.deterministic.best_side {
                common::Side::Yes => candidate.deterministic.p_det_yes,
                common::Side::No => candidate.deterministic.p_det_no,
            };
            let det_input = DeterministicInput {
                ticker: candidate.market.ticker.clone(),
                target_side: candidate.deterministic.best_side,
                target_price_cents: candidate.deterministic.best_price_cents,
                p_det_target,
                best_edge_cents: candidate.deterministic.best_edge_cents,
            };

            info!(
                "Analyzing candidate: {} score={:.3} edge={:.2}¢",
                candidate.market.ticker, candidate.complexity_score, candidate.deterministic.best_edge_cents
            );

            let request = ResearchRequest {
                request_id: Uuid::new_v4(),
                market_ticker: candidate.market.ticker.clone(),
                event_ticker: candidate.market.event_ticker.clone(),
                rules_primary: candidate.market.rules_primary.clone(),
                rules_secondary: Some(candidate.market.rules_secondary.clone()),
                candidate_snapshot: candidate.snapshot.clone(),
                allowed_urls: self.config.research.allowed_urls.clone(),
                as_of_ts_ms: chrono::Utc::now().timestamp_millis(),
            };

            let decision = if !self.budget_manager.can_spend_call(&candidate.market.ticker) {
                let (daily_used, daily_max, market_used, market_max) =
                    self.budget_manager.usage_snapshot(&candidate.market.ticker);
                self.write_event(json!({
                    "ts": now_iso(),
                    "kind": "research_budget_exhausted",
                    "ticker": candidate.market.ticker,
                    "daily_calls_used": daily_used,
                    "daily_calls_max": daily_max,
                    "market_calls_used": market_used,
                    "market_calls_max": market_max
                }));

                if self.config.research.llm_required_for_complex_markets {
                    Decision::Veto(decision_engine::VetoReason {
                        ticker: det_input.ticker.clone(),
                        reason: "LLM budget exhausted and llm_required_for_complex_markets=true".into(),
                    })
                } else {
                    self.decision_engine
                        .evaluate_deterministic(&det_input, "budget_exhausted")
                }
            } else {
                self.budget_manager.record_call(&candidate.market.ticker)?;
                llm_calls = llm_calls.saturating_add(1);

                self.write_event(json!({
                    "ts": now_iso(),
                    "kind": "research_requested",
                    "ticker": candidate.market.ticker,
                    "request_id": request.request_id,
                    "backend": if self.config.temporal.enabled { "temporal" } else { "direct_llm" }
                }));

                match self.request_research(request).await {
                Ok((workflow_id, research_response)) => {
                        let now_ms = chrono::Utc::now().timestamp_millis();
                        if now_ms - research_response.as_of_ts_ms
                            > self.config.research.staleness_budget_ms
                        {
                            llm_failures = llm_failures.saturating_add(1);
                            self.write_event(json!({
                                "ts": now_iso(),
                                "kind": "research_stale",
                                "ticker": candidate.market.ticker,
                                "workflow_id": workflow_id,
                                "as_of_ts_ms": research_response.as_of_ts_ms,
                                "now_ts_ms": now_ms,
                                "staleness_budget_ms": self.config.research.staleness_budget_ms
                            }));

                            if self.config.research.llm_required_for_complex_markets {
                                Decision::Veto(decision_engine::VetoReason {
                                    ticker: det_input.ticker.clone(),
                                    reason: "stale_research_response".into(),
                                })
                            } else {
                                self.decision_engine
                                    .evaluate_deterministic(&det_input, "stale_research_response")
                            }
                        } else {
                            self.write_event(json!({
                                "ts": now_iso(),
                                "kind": "research_accepted",
                                "ticker": candidate.market.ticker,
                                "workflow_id": workflow_id,
                                "risk_of_misresolution": format!("{:?}", research_response.risk_of_misresolution),
                                "confidence": research_response.confidence,
                                "uncertainty": research_response.uncertainty
                            }));
                            self.decision_engine
                                .evaluate_with_research(&det_input, &research_response)
                        }
                    }
                    Err(e) => {
                        llm_failures = llm_failures.saturating_add(1);
                        let code = Self::research_error_code(&e);
                        self.write_event(json!({
                            "ts": now_iso(),
                            "kind": "research_error",
                            "ticker": candidate.market.ticker,
                            "code": code,
                            "error": e.to_string()
                        }));
                        warn!("Research failed for {}: {}", candidate.market.ticker, e);

                        if self.config.research.llm_required_for_complex_markets {
                            Decision::Veto(decision_engine::VetoReason {
                                ticker: det_input.ticker.clone(),
                                reason: format!("{} and llm_required_for_complex_markets=true", code),
                            })
                        } else {
                            self.decision_engine.evaluate_deterministic(&det_input, code)
                        }
                    }
                }
            };

            match decision {
                Decision::Approve(intent) => {
                    let order_cost = intent.price_cents.saturating_mul(intent.size_contracts);
                    if !self.config.trading.shadow_mode {
                        if let Err(e) = self.risk_guard.check_buy(
                            &intent.ticker,
                            intent.price_cents,
                            intent.size_contracts,
                            &positions,
                            balance_cents,
                        ) {
                            vetoed = vetoed.saturating_add(1);
                            self.write_event(json!({
                                "ts": now_iso(),
                                "kind": "risk_rejected",
                                "ticker": intent.ticker,
                                "error": e.to_string()
                            }));
                            continue;
                        }
                    }

                    approved = approved.saturating_add(1);
                    self.write_event(json!({
                        "ts": now_iso(),
                        "kind": if self.config.trading.shadow_mode { "decision_shadow_trade" } else { "decision_live_trade" },
                        "ticker": intent.ticker,
                        "side": format!("{:?}", intent.side),
                        "price_cents": intent.price_cents,
                        "size_contracts": intent.size_contracts,
                        "edge_cents": intent.edge_cents,
                        "confidence": intent.confidence,
                        "reasons": intent.reasons
                    }));

                    match self.execution_engine.execute(intent).await? {
                        ExecutionOutcome::Placed => {
                            self.risk_guard.record_order();
                            // Conservative local updates to avoid over-trading in the same cycle.
                            balance_cents = balance_cents.saturating_sub(order_cost);
                            positions = self.client.get_positions().await.unwrap_or(positions);
                        }
                        ExecutionOutcome::Failed => {
                            vetoed = vetoed.saturating_add(1);
                        }
                        ExecutionOutcome::ShadowSkipped | ExecutionOutcome::LiveDisabled => {}
                    }
                }
                Decision::Veto(reason) => {
                    vetoed = vetoed.saturating_add(1);
                    self.write_event(json!({
                        "ts": now_iso(),
                        "kind": "decision_veto",
                        "ticker": reason.ticker,
                        "reason": reason.reason
                    }));
                }
            };
        }

        self.write_event(json!({
            "ts": now_iso(),
            "kind": "strategy_cycle_summary",
            "approved": approved,
            "vetoed": vetoed,
            "llm_calls": llm_calls,
            "llm_failures": llm_failures
        }));
        Ok(())
    }
}

fn parse_risk_level(raw: &str) -> Result<RiskLevel> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "low" => Ok(RiskLevel::Low),
        "medium" => Ok(RiskLevel::Medium),
        "high" => Ok(RiskLevel::High),
        "critical" => Ok(RiskLevel::Critical),
        other => bail!(
            "invalid research.rules_risk_veto_level '{}'; expected low|medium|high|critical",
            other
        ),
    }
}
