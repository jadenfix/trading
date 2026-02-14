use std::collections::HashMap;
use std::fs::{create_dir_all, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{Duration as ChronoDuration, SecondsFormat, Utc};
use clap::{Parser, ValueEnum};
use common::{Action, FeeSchedule, OrderIntent, Side};
use kalshi_client::{KalshiAuth, KalshiRestClient};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use tokio::time::{sleep, Duration, Instant};
use tracing::{error, info, warn};
use uuid::Uuid;

const BOT_TRADE_DIR: &str = "sports-agent";
const SPORT_KEYWORDS: [&str; 13] = [
    "nfl",
    "nba",
    "mlb",
    "nhl",
    "ncaa",
    "wnba",
    "soccer",
    "football",
    "basketball",
    "baseball",
    "hockey",
    "playoffs",
    "super bowl",
];

#[derive(Debug, Clone, Copy, ValueEnum, Eq, PartialEq)]
enum ExecutionMode {
    Hitl,
    AutoUltraStrict,
}

impl ExecutionMode {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Hitl => "hitl",
            Self::AutoUltraStrict => "auto_ultra_strict",
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "sports-agent-worker",
    about = "Sports analytics research + execution workflow"
)]
struct Cli {
    #[arg(long, value_enum, default_value_t = ExecutionMode::Hitl)]
    mode: ExecutionMode,

    #[arg(long, default_value_t = 45)]
    interval_secs: u64,

    #[arg(long, default_value_t = 300)]
    hitl_wait_secs: u64,

    #[arg(long, default_value_t = 1)]
    order_size: i64,

    #[arg(long)]
    dry_run: bool,

    #[arg(long)]
    check_auth: bool,

    #[arg(long, default_value_t = 8)]
    min_net_edge_cents: i64,

    #[arg(long, default_value_t = 7000)]
    min_lower_bound_bps: i64,

    #[arg(long, default_value_t = 250)]
    min_clv_proxy_bps: i64,

    #[arg(long, default_value_t = 500)]
    min_open_interest: i64,

    #[arg(long, default_value_t = 100)]
    min_volume_24h: i64,

    #[arg(long, default_value_t = 10)]
    max_spread_cents: i64,

    #[arg(long, default_value_t = 1)]
    slippage_buffer_cents: i64,

    #[arg(long, default_value_t = 0.08)]
    max_signal_disagreement: f64,

    #[arg(long, default_value_t = 80)]
    synthetic_samples: i64,

    #[arg(long, default_value_t = 2.0)]
    prior_alpha: f64,

    #[arg(long, default_value_t = 2.0)]
    prior_beta: f64,

    #[arg(long, default_value = "http://127.0.0.1:8787")]
    broker_base_url: String,

    #[arg(long, default_value = "nfl")]
    sportsdataio_league: String,

    #[arg(long, default_value = "americanfootball_nfl")]
    odds_sport_key: String,

    #[arg(long, default_value_t = 99.0)]
    confidence_z: f64,
}

#[derive(Debug, Deserialize)]
struct SportsDataGame {
    #[serde(rename = "HomeTeam", default)]
    home_team: String,
    #[serde(rename = "AwayTeam", default)]
    away_team: String,
    #[serde(rename = "HomeScore", default)]
    home_score: i64,
    #[serde(rename = "AwayScore", default)]
    away_score: i64,
    #[serde(rename = "Status", default)]
    status: String,
}

impl SportsDataGame {
    fn is_final(&self) -> bool {
        let s = self.status.to_ascii_lowercase();
        s.contains("final") || s.contains("closed")
    }
}

#[derive(Debug, Deserialize)]
struct OddsEvent {
    #[serde(default)]
    home_team: String,
    #[serde(default)]
    away_team: String,
    #[serde(default)]
    bookmakers: Vec<OddsBookmaker>,
}

#[derive(Debug, Deserialize)]
struct OddsBookmaker {
    #[serde(default)]
    markets: Vec<OddsMarket>,
}

#[derive(Debug, Deserialize)]
struct OddsMarket {
    #[serde(default)]
    key: String,
    #[serde(default)]
    outcomes: Vec<OddsOutcome>,
}

#[derive(Debug, Deserialize)]
struct OddsOutcome {
    #[serde(default)]
    name: String,
    #[serde(default)]
    price: f64,
}

#[derive(Debug, Clone)]
struct PosteriorStats {
    mean_yes: f64,
    lower_yes: f64,
    upper_yes: f64,
    lower_no: f64,
}

#[derive(Debug, Clone)]
struct Candidate {
    ticker: String,
    event_ticker: String,
    title: String,
    matched_team: String,
    side: Side,
    ask_cents: i64,
    bid_cents: i64,
    spread_cents: i64,
    p_odds: f64,
    p_stats: Option<f64>,
    p_signal: f64,
    posterior_mean: f64,
    lower_bound: f64,
    ev_net_cents: f64,
    fee_cents_est: i64,
    clv_proxy_bps: f64,
    signal_disagreement: f64,
    open_interest: i64,
    volume_24h: i64,
    rationale: Vec<String>,
}

#[derive(Debug, Clone)]
struct TraceContext {
    trace_id: String,
    workflow_id: String,
    mode: ExecutionMode,
    cycle: u64,
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

    fn write_event(&mut self, event: Value) {
        let result = (|| -> std::io::Result<()> {
            self.rotate_if_needed()?;
            let line = serde_json::to_string(&event).unwrap_or_else(|_| "{}".to_string());
            writeln!(self.file, "{}", line)?;
            self.file.flush()?;
            Ok(())
        })();

        if let Err(e) = result {
            warn!("sports-agent journal write failed: {}", e);
        }
    }

    fn dir(&self) -> &Path {
        &self.dir
    }
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
            return PathBuf::from(trimmed).join(BOT_TRADE_DIR);
        }
    }

    if let Some(root) = resolve_repo_root() {
        return root.join("TRADES").join(BOT_TRADE_DIR);
    }

    PathBuf::from("TRADES").join(BOT_TRADE_DIR)
}

fn env_bool(name: &str, default_value: bool) -> bool {
    let Ok(raw) = std::env::var(name) else {
        return default_value;
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => true,
        "0" | "false" | "no" | "off" => false,
        _ => default_value,
    }
}

fn normalize_text(input: &str) -> String {
    input
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch.is_ascii_whitespace() {
                ch.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn market_text(market: &common::MarketInfo) -> String {
    format!(
        "{} {} {} {} {}",
        market.title,
        market.subtitle,
        market.event_ticker,
        market.rules_primary,
        market.rules_secondary
    )
}

fn market_is_sports(market: &common::MarketInfo) -> bool {
    let text = normalize_text(&market_text(market));
    SPORT_KEYWORDS.iter().any(|kw| text.contains(kw))
}

fn side_label(side: Side) -> &'static str {
    match side {
        Side::Yes => "yes",
        Side::No => "no",
    }
}

fn compose_trace_event(trace: &TraceContext, kind: &str, payload: Value) -> Value {
    let mut obj = match payload {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    obj.insert("ts".to_string(), json!(now_iso()));
    obj.insert("bot".to_string(), json!("sports-agent"));
    obj.insert("kind".to_string(), json!(kind));
    obj.insert("trace_id".to_string(), json!(trace.trace_id));
    obj.insert("workflow_id".to_string(), json!(trace.workflow_id));
    obj.insert("mode".to_string(), json!(trace.mode.as_str()));
    obj.insert("cycle".to_string(), json!(trace.cycle));
    Value::Object(obj)
}

fn z_score_from_confidence_percent(confidence_percent: f64) -> f64 {
    if confidence_percent >= 99.0 {
        2.326
    } else if confidence_percent >= 97.5 {
        1.96
    } else if confidence_percent >= 95.0 {
        1.645
    } else {
        1.282
    }
}

fn posterior_from_signal(
    p_signal_yes: f64,
    prior_alpha: f64,
    prior_beta: f64,
    synthetic_samples: i64,
    z_score: f64,
) -> PosteriorStats {
    let n = synthetic_samples.max(8) as f64;
    let bounded = p_signal_yes.clamp(0.01, 0.99);

    let alpha = prior_alpha.max(0.01) + bounded * n;
    let beta = prior_beta.max(0.01) + (1.0 - bounded) * n;

    let mean_yes = alpha / (alpha + beta);
    let variance = (alpha * beta) / (((alpha + beta).powi(2)) * (alpha + beta + 1.0));
    let sigma = variance.max(0.0).sqrt();

    let lower_yes = (mean_yes - z_score * sigma).clamp(0.0, 1.0);
    let upper_yes = (mean_yes + z_score * sigma).clamp(0.0, 1.0);
    let lower_no = (1.0 - upper_yes).clamp(0.0, 1.0);

    PosteriorStats {
        mean_yes,
        lower_yes,
        upper_yes,
        lower_no,
    }
}

fn american_or_decimal_to_prob(price: f64) -> Option<f64> {
    if !price.is_finite() {
        return None;
    }

    if price >= 100.0 {
        return Some(100.0 / (price + 100.0));
    }
    if price <= -100.0 {
        let abs = price.abs();
        return Some(abs / (abs + 100.0));
    }

    if price > 1.0 {
        return Some((1.0 / price).clamp(0.001, 0.999));
    }

    None
}

fn extract_event_probabilities(event: &OddsEvent) -> HashMap<String, f64> {
    let mut aggregate: HashMap<String, (f64, i64)> = HashMap::new();

    for bookmaker in &event.bookmakers {
        for market in &bookmaker.markets {
            if market.key != "h2h" {
                continue;
            }

            let mut raw_probs: Vec<(String, f64)> = Vec::new();
            for outcome in &market.outcomes {
                let Some(prob) = american_or_decimal_to_prob(outcome.price) else {
                    continue;
                };
                raw_probs.push((outcome.name.clone(), prob));
            }

            if raw_probs.len() < 2 {
                continue;
            }

            let total: f64 = raw_probs.iter().map(|(_, p)| *p).sum();
            if total <= 0.0 {
                continue;
            }

            for (name, prob) in raw_probs {
                let normalized = (prob / total).clamp(0.001, 0.999);
                let entry = aggregate.entry(name).or_insert((0.0, 0));
                entry.0 += normalized;
                entry.1 += 1;
            }
        }
    }

    aggregate
        .into_iter()
        .filter_map(|(team, (sum, count))| {
            if count > 0 {
                Some((team, (sum / count as f64).clamp(0.001, 0.999)))
            } else {
                None
            }
        })
        .collect()
}

fn logistic(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}

fn build_team_ratings(games: &[SportsDataGame]) -> HashMap<String, f64> {
    let mut accum: HashMap<String, (f64, i64)> = HashMap::new();

    for game in games {
        if !game.is_final() {
            continue;
        }
        if game.home_team.is_empty() || game.away_team.is_empty() {
            continue;
        }

        let home_delta = (game.home_score - game.away_score) as f64;
        let away_delta = -home_delta;

        let home_key = normalize_text(&game.home_team);
        let away_key = normalize_text(&game.away_team);

        let home_entry = accum.entry(home_key).or_insert((0.0, 0));
        home_entry.0 += home_delta;
        home_entry.1 += 1;

        let away_entry = accum.entry(away_key).or_insert((0.0, 0));
        away_entry.0 += away_delta;
        away_entry.1 += 1;
    }

    accum
        .into_iter()
        .filter_map(|(team, (sum, count))| {
            if count <= 0 {
                None
            } else {
                Some((team, sum / count as f64))
            }
        })
        .collect()
}

fn team_probability_from_ratings(
    event: &OddsEvent,
    outcome_name: &str,
    ratings: &HashMap<String, f64>,
) -> Option<f64> {
    let home_key = normalize_text(&event.home_team);
    let away_key = normalize_text(&event.away_team);
    let home_rating = ratings.get(&home_key)?;
    let away_rating = ratings.get(&away_key)?;

    let home_prob = logistic((home_rating - away_rating) / 10.0).clamp(0.001, 0.999);
    let outcome_key = normalize_text(outcome_name);

    if outcome_key == home_key {
        Some(home_prob)
    } else if outcome_key == away_key {
        Some(1.0 - home_prob)
    } else {
        None
    }
}

async fn fetch_sportsdata_games(
    http: &reqwest::Client,
    league: &str,
    api_key: &str,
    day: chrono::NaiveDate,
) -> Result<Vec<SportsDataGame>> {
    let date_key = day.format("%Y-%b-%d").to_string().to_ascii_uppercase();
    let url = format!(
        "https://api.sportsdata.io/v3/{}/scores/json/GamesByDate/{}",
        league, date_key
    );

    let response = http
        .get(url)
        .header("Ocp-Apim-Subscription-Key", api_key)
        .send()
        .await
        .context("sportsdata request failed")?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        bail!("sportsdata http status={} body={}", status.as_u16(), body);
    }

    let payload: Vec<SportsDataGame> = response.json().await.context("sportsdata decode failed")?;

    Ok(payload)
}

async fn fetch_the_odds_events(
    http: &reqwest::Client,
    sport_key: &str,
    api_key: &str,
) -> Result<Vec<OddsEvent>> {
    let url = format!(
        "https://api.the-odds-api.com/v4/sports/{}/odds?regions=us&markets=h2h&oddsFormat=american&apiKey={}",
        sport_key, api_key
    );

    let response = http
        .get(url)
        .send()
        .await
        .context("the-odds request failed")?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        bail!("the-odds http status={} body={}", status.as_u16(), body);
    }

    let payload: Vec<OddsEvent> = response.json().await.context("the-odds decode failed")?;

    Ok(payload)
}

fn evaluate_market_candidates(
    cli: &Cli,
    markets: &[common::MarketInfo],
    odds_events: &[OddsEvent],
    team_ratings: &HashMap<String, f64>,
) -> Vec<Candidate> {
    let fee_schedule = FeeSchedule::weather();
    let z_score = z_score_from_confidence_percent(cli.confidence_z);
    let mut best_per_market: HashMap<String, Candidate> = HashMap::new();

    for market in markets {
        if !market_is_sports(market) {
            continue;
        }
        if market.open_interest < cli.min_open_interest || market.volume_24h < cli.min_volume_24h {
            continue;
        }

        let text = normalize_text(&market_text(market));
        if text.is_empty() {
            continue;
        }

        for event in odds_events {
            let probs = extract_event_probabilities(event);
            if probs.is_empty() {
                continue;
            }

            for (team_name, p_odds) in probs {
                let team_key = normalize_text(&team_name);
                if team_key.len() < 3 || !text.contains(&team_key) {
                    continue;
                }

                let p_stats = team_probability_from_ratings(event, &team_name, team_ratings);
                let signal_disagreement = p_stats.map(|v| (v - p_odds).abs()).unwrap_or(0.0);
                let p_signal = if let Some(stats) = p_stats {
                    (0.65 * p_odds + 0.35 * stats).clamp(0.001, 0.999)
                } else {
                    p_odds
                };

                let scaled_samples = ((cli.synthetic_samples as f64)
                    * (1.0 - signal_disagreement).max(0.35))
                .round() as i64;
                let posterior = posterior_from_signal(
                    p_signal,
                    cli.prior_alpha,
                    cli.prior_beta,
                    scaled_samples,
                    z_score,
                );

                let yes_spread = market.yes_ask.saturating_sub(market.yes_bid);
                let yes_fee = fee_schedule.per_contract_taker_fee(market.yes_ask).ceil() as i64;
                let yes_ev = posterior.mean_yes * 100.0
                    - market.yes_ask as f64
                    - yes_fee as f64
                    - cli.slippage_buffer_cents as f64;
                let yes_clv =
                    (p_signal - market.yes_ask as f64 / 100.0).clamp(-1.0, 1.0) * 10_000.0;

                let no_spread = market.no_ask.saturating_sub(market.no_bid);
                let no_fee = fee_schedule.per_contract_taker_fee(market.no_ask).ceil() as i64;
                let no_ev = (1.0 - posterior.mean_yes) * 100.0
                    - market.no_ask as f64
                    - no_fee as f64
                    - cli.slippage_buffer_cents as f64;
                let no_clv =
                    ((1.0 - p_signal) - market.no_ask as f64 / 100.0).clamp(-1.0, 1.0) * 10_000.0;

                let yes_candidate = Candidate {
                    ticker: market.ticker.clone(),
                    event_ticker: market.event_ticker.clone(),
                    title: market.title.clone(),
                    matched_team: team_name.clone(),
                    side: Side::Yes,
                    ask_cents: market.yes_ask,
                    bid_cents: market.yes_bid,
                    spread_cents: yes_spread,
                    p_odds,
                    p_stats,
                    p_signal,
                    posterior_mean: posterior.mean_yes,
                    lower_bound: posterior.lower_yes,
                    ev_net_cents: yes_ev,
                    fee_cents_est: yes_fee,
                    clv_proxy_bps: yes_clv,
                    signal_disagreement,
                    open_interest: market.open_interest,
                    volume_24h: market.volume_24h,
                    rationale: vec![
                        format!("team_match={}", team_name),
                        format!("posterior_yes={:.4}", posterior.mean_yes),
                        format!("lower_yes={:.4}", posterior.lower_yes),
                        format!("odds_prob={:.4}", p_odds),
                        format!("stats_prob={:.4}", p_stats.unwrap_or(-1.0)),
                    ],
                };

                let no_candidate = Candidate {
                    ticker: market.ticker.clone(),
                    event_ticker: market.event_ticker.clone(),
                    title: market.title.clone(),
                    matched_team: team_name,
                    side: Side::No,
                    ask_cents: market.no_ask,
                    bid_cents: market.no_bid,
                    spread_cents: no_spread,
                    p_odds,
                    p_stats,
                    p_signal,
                    posterior_mean: 1.0 - posterior.mean_yes,
                    lower_bound: posterior.lower_no,
                    ev_net_cents: no_ev,
                    fee_cents_est: no_fee,
                    clv_proxy_bps: no_clv,
                    signal_disagreement,
                    open_interest: market.open_interest,
                    volume_24h: market.volume_24h,
                    rationale: vec![
                        format!("team_match={}", market.title),
                        format!("posterior_no={:.4}", 1.0 - posterior.mean_yes),
                        format!("lower_no={:.4}", posterior.lower_no),
                        format!("odds_prob={:.4}", p_odds),
                        format!("stats_prob={:.4}", p_stats.unwrap_or(-1.0)),
                    ],
                };

                let chosen = if yes_candidate.ev_net_cents >= no_candidate.ev_net_cents {
                    yes_candidate
                } else {
                    no_candidate
                };

                if !(1..=99).contains(&chosen.ask_cents) {
                    continue;
                }

                let replace = best_per_market
                    .get(&chosen.ticker)
                    .map(|existing| chosen.ev_net_cents > existing.ev_net_cents)
                    .unwrap_or(true);
                if replace {
                    best_per_market.insert(chosen.ticker.clone(), chosen);
                }
            }
        }
    }

    best_per_market.into_values().collect()
}

fn candidate_score(candidate: &Candidate) -> f64 {
    let liquidity_score = ((candidate.open_interest.max(1) as f64).ln() * 0.7)
        + ((candidate.volume_24h.max(1) as f64).ln() * 0.3);

    candidate.ev_net_cents * 3.0 + candidate.lower_bound * 100.0 + liquidity_score
}

fn passes_hitl_gate(cli: &Cli, candidate: &Candidate) -> bool {
    candidate.ev_net_cents >= 2.0
        && candidate.spread_cents <= cli.max_spread_cents
        && candidate.clv_proxy_bps >= 75.0
        && candidate.signal_disagreement <= 0.20
}

fn passes_auto_gate(cli: &Cli, candidate: &Candidate) -> bool {
    let lower_bound_floor = cli.min_lower_bound_bps as f64 / 10_000.0;

    candidate.lower_bound >= lower_bound_floor
        && candidate.ev_net_cents >= cli.min_net_edge_cents as f64
        && candidate.clv_proxy_bps >= cli.min_clv_proxy_bps as f64
        && candidate.spread_cents <= cli.max_spread_cents
        && candidate.signal_disagreement <= cli.max_signal_disagreement
}

fn recommendation_payload(trace: &TraceContext, cli: &Cli, candidate: &Candidate) -> Value {
    json!({
        "trace_id": trace.trace_id,
        "workflow_id": trace.workflow_id,
        "ticker": candidate.ticker,
        "event_ticker": candidate.event_ticker,
        "title": candidate.title,
        "matched_team": candidate.matched_team,
        "side": side_label(candidate.side),
        "price_limit_cents": candidate.ask_cents,
        "size_contracts": cli.order_size,
        "p_win_posterior": candidate.posterior_mean,
        "p_win_lower_bound": candidate.lower_bound,
        "ev_net_cents": candidate.ev_net_cents,
        "fee_cents_est": candidate.fee_cents_est,
        "clv_proxy_bps": candidate.clv_proxy_bps,
        "mode": trace.mode.as_str(),
        "signals": {
            "p_odds": candidate.p_odds,
            "p_stats": candidate.p_stats,
            "p_signal": candidate.p_signal,
            "signal_disagreement": candidate.signal_disagreement
        },
        "rationale": candidate.rationale,
    })
}

async fn broker_register(
    http: &reqwest::Client,
    base_url: &str,
    trace: &TraceContext,
    status: &str,
    requires_approval: bool,
    recommendation: &Value,
) {
    let url = format!("{}/workflows/register", base_url.trim_end_matches('/'));
    let payload = json!({
        "workflow_id": trace.workflow_id,
        "trace_id": trace.trace_id,
        "source_bot": "sports-agent",
        "mode": trace.mode.as_str(),
        "status": status,
        "requires_approval": requires_approval,
        "recommendation": recommendation,
    });

    if let Err(e) = http.post(url).json(&payload).send().await {
        warn!("broker register failed: {}", e);
    }
}

async fn broker_complete(
    http: &reqwest::Client,
    base_url: &str,
    workflow_id: &str,
    status: &str,
    result: &Value,
) {
    let url = format!(
        "{}/workflows/{}/complete",
        base_url.trim_end_matches('/'),
        workflow_id
    );

    let payload = json!({
        "status": status,
        "result": result,
    });

    if let Err(e) = http.post(url).json(&payload).send().await {
        warn!("broker complete failed: {}", e);
    }
}

async fn broker_append_event(
    http: &reqwest::Client,
    base_url: &str,
    workflow_id: &str,
    event: &Value,
) {
    let url = format!(
        "{}/workflows/{}/events",
        base_url.trim_end_matches('/'),
        workflow_id
    );

    if let Err(e) = http.post(url).json(event).send().await {
        warn!("broker event append failed: {}", e);
    }
}

async fn broker_is_approved(http: &reqwest::Client, base_url: &str, workflow_id: &str) -> bool {
    let url = format!(
        "{}/execution/{}/approval",
        base_url.trim_end_matches('/'),
        workflow_id
    );

    let response = match http.get(url).send().await {
        Ok(resp) => resp,
        Err(e) => {
            warn!("approval poll failed: {}", e);
            return false;
        }
    };

    if !response.status().is_success() {
        return false;
    }

    let body = match response.json::<Value>().await {
        Ok(payload) => payload,
        Err(e) => {
            warn!("approval decode failed: {}", e);
            return false;
        }
    };

    body.get("approved")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

async fn wait_for_approval(
    http: &reqwest::Client,
    base_url: &str,
    workflow_id: &str,
    timeout_secs: u64,
) -> bool {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);

    loop {
        if broker_is_approved(http, base_url, workflow_id).await {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        sleep(Duration::from_secs(2)).await;
    }
}

async fn execute_candidate(
    cli: &Cli,
    client: &KalshiRestClient,
    candidate: &Candidate,
) -> Result<Value> {
    let order_cost = candidate.ask_cents.saturating_mul(cli.order_size) + candidate.fee_cents_est;

    let balance = client.get_balance().await.context("balance fetch failed")?;
    if balance < order_cost {
        bail!(
            "insufficient balance: need={} available={}",
            order_cost,
            balance
        );
    }

    if cli.dry_run {
        return Ok(json!({
            "status": "dry_run_simulated",
            "order_cost_cents": order_cost,
            "balance_cents": balance
        }));
    }

    let intent = OrderIntent {
        ticker: candidate.ticker.clone(),
        side: candidate.side,
        action: Action::Buy,
        price_cents: candidate.ask_cents,
        count: cli.order_size,
        reason: format!(
            "sports_agent {} signal edge={:.2} lower={:.3}",
            cli.mode.as_str(),
            candidate.ev_net_cents,
            candidate.lower_bound
        ),
        estimated_fee_cents: candidate.fee_cents_est,
        confidence: candidate.posterior_mean,
    };

    let response = client
        .create_order(&intent)
        .await
        .context("order placement failed")?;

    Ok(json!({
        "status": "order_placed",
        "order_id": response.order.order_id,
        "client_order_id": response.order.client_order_id,
        "fill_count": response.order.fill_count,
        "remaining_count": response.order.remaining_count,
        "order_status": response.order.status,
        "taker_fees": response.order.taker_fees,
        "maker_fees": response.order.maker_fees,
        "balance_cents": balance,
        "order_cost_cents": order_cost
    }))
}

async fn run_cycle(
    cli: &Cli,
    trace: &TraceContext,
    journal: &mut TradeJournal,
    rest_client: &KalshiRestClient,
    http_client: &reqwest::Client,
    sports_data_key: Option<&str>,
    odds_key: Option<&str>,
) -> Result<()> {
    journal.write_event(compose_trace_event(
        trace,
        "strategy_cycle_start",
        json!({
            "broker_base_url": cli.broker_base_url,
            "interval_secs": cli.interval_secs,
            "dry_run": cli.dry_run,
        }),
    ));

    let Some(odds_api_key) = odds_key else {
        let message = "THE_ODDS_API_KEY is required for sports-agent";
        journal.write_event(compose_trace_event(
            trace,
            "external_data_error",
            json!({"provider": "the-odds", "error": message}),
        ));
        bail!(message);
    };

    let odds_events = fetch_the_odds_events(http_client, &cli.odds_sport_key, odds_api_key).await?;
    journal.write_event(compose_trace_event(
        trace,
        "external_data_fetch",
        json!({"provider": "the-odds", "events": odds_events.len()}),
    ));

    let mut sports_games: Vec<SportsDataGame> = Vec::new();
    if let Some(key) = sports_data_key {
        let today = Utc::now().date_naive();
        for offset in 0..2 {
            let target = today - ChronoDuration::days(offset);
            match fetch_sportsdata_games(http_client, &cli.sportsdataio_league, key, target).await {
                Ok(mut payload) => sports_games.append(&mut payload),
                Err(e) => {
                    warn!("sportsdata fetch failed for {}: {}", target, e);
                    journal.write_event(compose_trace_event(
                        trace,
                        "external_data_error",
                        json!({
                            "provider": "sportsdataio",
                            "day": target.to_string(),
                            "error": e.to_string()
                        }),
                    ));
                }
            }
        }
    } else {
        journal.write_event(compose_trace_event(
            trace,
            "external_data_error",
            json!({
                "provider": "sportsdataio",
                "error": "SPORTS_DATA_IO_API_KEY not set; stats signal disabled"
            }),
        ));
    }

    let team_ratings = build_team_ratings(&sports_games);
    journal.write_event(compose_trace_event(
        trace,
        "external_data_fetch",
        json!({
            "provider": "sportsdataio",
            "games": sports_games.len(),
            "rated_teams": team_ratings.len()
        }),
    ));

    let markets = rest_client
        .get_markets(None, Some("open"), 200)
        .await
        .context("market fetch failed")?;

    journal.write_event(compose_trace_event(
        trace,
        "market_snapshot",
        json!({"open_markets": markets.len()}),
    ));

    let candidates = evaluate_market_candidates(cli, &markets, &odds_events, &team_ratings);
    journal.write_event(compose_trace_event(
        trace,
        "candidate_generation",
        json!({"candidates": candidates.len()}),
    ));

    let filtered: Vec<Candidate> = candidates
        .into_iter()
        .filter(|candidate| {
            if cli.mode == ExecutionMode::AutoUltraStrict {
                passes_auto_gate(cli, candidate)
            } else {
                passes_hitl_gate(cli, candidate)
            }
        })
        .collect();

    if filtered.is_empty() {
        journal.write_event(compose_trace_event(
            trace,
            "no_recommendation",
            json!({
                "reason": if cli.mode == ExecutionMode::AutoUltraStrict {
                    "no_candidate_met_auto_ultra_strict_gate"
                } else {
                    "no_candidate_met_hitl_gate"
                }
            }),
        ));
        broker_complete(
            http_client,
            &cli.broker_base_url,
            &trace.workflow_id,
            "completed",
            &json!({"status": "no_recommendation"}),
        )
        .await;
        return Ok(());
    }

    let best = filtered
        .into_iter()
        .max_by(|left, right| {
            candidate_score(left)
                .partial_cmp(&candidate_score(right))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .expect("filtered is non-empty");

    let recommendation = recommendation_payload(trace, cli, &best);
    journal.write_event(compose_trace_event(
        trace,
        "recommendation_generated",
        recommendation.clone(),
    ));

    if cli.mode == ExecutionMode::Hitl {
        broker_register(
            http_client,
            &cli.broker_base_url,
            trace,
            "awaiting_approval",
            true,
            &recommendation,
        )
        .await;
        broker_append_event(
            http_client,
            &cli.broker_base_url,
            &trace.workflow_id,
            &json!({"kind": "recommendation_awaiting_approval", "trace_id": trace.trace_id}),
        )
        .await;

        journal.write_event(compose_trace_event(
            trace,
            "recommendation_awaiting_approval",
            json!({"timeout_secs": cli.hitl_wait_secs}),
        ));

        let approved = wait_for_approval(
            http_client,
            &cli.broker_base_url,
            &trace.workflow_id,
            cli.hitl_wait_secs,
        )
        .await;

        if !approved {
            journal.write_event(compose_trace_event(
                trace,
                "approval_timeout",
                json!({"timeout_secs": cli.hitl_wait_secs}),
            ));
            broker_complete(
                http_client,
                &cli.broker_base_url,
                &trace.workflow_id,
                "awaiting_approval",
                &json!({"status": "approval_timeout"}),
            )
            .await;
            return Ok(());
        }

        journal.write_event(compose_trace_event(
            trace,
            "execution_approved",
            json!({"approved": true}),
        ));
    } else {
        broker_register(
            http_client,
            &cli.broker_base_url,
            trace,
            "running",
            false,
            &recommendation,
        )
        .await;
    }

    let execution_result = execute_candidate(cli, rest_client, &best).await;
    match execution_result {
        Ok(result) => {
            journal.write_event(compose_trace_event(
                trace,
                "execution_result",
                result.clone(),
            ));
            let final_status = if result
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or_default()
                == "order_placed"
            {
                "executed"
            } else {
                "completed"
            };
            broker_complete(
                http_client,
                &cli.broker_base_url,
                &trace.workflow_id,
                final_status,
                &result,
            )
            .await;
        }
        Err(e) => {
            journal.write_event(compose_trace_event(
                trace,
                "execution_error",
                json!({"error": e.to_string()}),
            ));
            broker_complete(
                http_client,
                &cli.broker_base_url,
                &trace.workflow_id,
                "failed",
                &json!({"error": e.to_string()}),
            )
            .await;
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "sports_agent_worker=info,kalshi_client=info".into()),
        )
        .init();

    let cli = Cli::parse();

    info!(
        "sports-agent starting mode={} interval={}s dry_run={}",
        cli.mode.as_str(),
        cli.interval_secs,
        cli.dry_run
    );

    let use_demo = env_bool("KALSHI_USE_DEMO", false);
    let auth = KalshiAuth::from_env().context("kalshi auth init failed")?;
    let rest_client = KalshiRestClient::new(auth, use_demo);

    let trades_dir = resolve_trades_dir();
    let mut journal =
        TradeJournal::open(trades_dir).context("failed to open sports-agent journal")?;
    info!("sports-agent journal path: {}", journal.dir().display());

    let http_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .context("failed to build http client")?;

    let sports_data_key = std::env::var("SPORTS_DATA_IO_API_KEY").ok();
    let odds_key = std::env::var("THE_ODDS_API_KEY").ok();

    if cli.check_auth {
        match rest_client.get_balance().await {
            Ok(balance) => {
                info!("kalshi auth check ok: balance={} cents", balance);
                journal.write_event(json!({
                    "ts": now_iso(),
                    "bot": "sports-agent",
                    "kind": "auth_check",
                    "status": "ok",
                    "balance_cents": balance,
                    "mode": cli.mode.as_str(),
                    "use_demo": use_demo,
                }));
                return Ok(());
            }
            Err(e) => {
                error!("kalshi auth check failed: {}", e);
                journal.write_event(json!({
                    "ts": now_iso(),
                    "bot": "sports-agent",
                    "kind": "auth_check",
                    "status": "error",
                    "error": e.to_string(),
                    "mode": cli.mode.as_str(),
                    "use_demo": use_demo,
                }));
                bail!("auth check failed");
            }
        }
    }

    let mut cycle = 0u64;
    loop {
        cycle = cycle.saturating_add(1);

        let workflow_id = format!(
            "sports-research-{}-{}",
            Utc::now().format("%Y%m%d%H%M%S"),
            Uuid::new_v4().simple()
        );
        let trace = TraceContext {
            trace_id: workflow_id.clone(),
            workflow_id,
            mode: cli.mode,
            cycle,
        };

        journal.write_event(compose_trace_event(
            &trace,
            "workflow_registered",
            json!({"cycle_interval_secs": cli.interval_secs}),
        ));

        if let Err(e) = run_cycle(
            &cli,
            &trace,
            &mut journal,
            &rest_client,
            &http_client,
            sports_data_key.as_deref(),
            odds_key.as_deref(),
        )
        .await
        {
            error!("cycle {} failed: {}", cycle, e);
            journal.write_event(compose_trace_event(
                &trace,
                "strategy_cycle_error",
                json!({"error": e.to_string()}),
            ));
        }

        sleep(Duration::from_secs(cli.interval_secs)).await;
    }
}
