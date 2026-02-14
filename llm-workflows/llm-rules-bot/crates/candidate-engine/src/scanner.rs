use crate::types::{DeterministicSignal, ResearchCandidate};
use crate::scorer::compute_complexity_score;
use common::Side;
use kalshi_client::KalshiRestClient;
use serde_json::json;
use tracing::info;

fn clamp_prob(value: f64) -> f64 {
    value.clamp(0.01, 0.99)
}

fn compute_deterministic_signal(market: &common::MarketInfo) -> Option<DeterministicSignal> {
    if !(1..=99).contains(&market.yes_ask) || !(1..=99).contains(&market.no_ask) {
        return None;
    }

    let p_det_yes = if (1..=99).contains(&market.last_price) {
        clamp_prob(market.last_price as f64 / 100.0)
    } else {
        // Fallback to implied mid when last price is unavailable/invalid.
        let mid = (market.yes_bid + market.yes_ask) as f64 / 200.0;
        clamp_prob(mid)
    };
    let p_det_no = 1.0 - p_det_yes;

    let edge_yes_cents = p_det_yes * 100.0 - market.yes_ask as f64;
    let edge_no_cents = p_det_no * 100.0 - market.no_ask as f64;

    let (best_side, best_price_cents, best_edge_cents) = if edge_yes_cents >= edge_no_cents {
        (Side::Yes, market.yes_ask, edge_yes_cents)
    } else {
        (Side::No, market.no_ask, edge_no_cents)
    };

    Some(DeterministicSignal {
        p_det_yes,
        p_det_no,
        edge_yes_cents,
        edge_no_cents,
        best_side,
        best_price_cents,
        best_edge_cents,
    })
}

pub struct Scanner {
    client: KalshiRestClient,
}

impl Scanner {
    pub fn new(client: KalshiRestClient) -> Self {
        Self { client }
    }

    pub async fn scan_markets(&self, max_days_to_expiry: i64) -> anyhow::Result<Vec<ResearchCandidate>> {
        // Fetch active markets
        // Using "active" status or similar? The client has `get_markets` with status filter.
        // Assuming "open" or "active" is the status we want.
        let markets = self.client.get_markets(None, Some("active"), 1000).await?;
        
        info!("Scanned {} active markets", markets.len());

        let mut candidates = Vec::new();
        let now = chrono::Utc::now();

        for market in markets {
            // Filter by expiration
            if let Some(expiration) = market.expiration_time {
                let days_to_expiry = (expiration - now).num_days();
                if days_to_expiry > max_days_to_expiry {
                    continue;
                }
            } else {
                // If no expiration, skip? or include?
                // Most Kalshi markets have expiration.
                // Safest to include if unknown, or skip if strict.
                // Let's skip if we want strict "within 10 days".
                continue;
            }

            let deterministic = match compute_deterministic_signal(&market) {
                Some(signal) => signal,
                None => continue,
            };

            let (score, reasons) = compute_complexity_score(&market);

            // Filter for minimal edge/liquidity? Or just everything above complexity threshold?
            // Plan says: "Universe Scanner: pulls open markets and computes deterministic complexity score"
            // "Candidate Engine: computes baseline deterministic edge... emits ResearchCandidate"
            
            // For now, let's process all and let Main filter or Decision filter.
            // Or apply a basic complexity filter here if we only want "rules-risk" candidates.
            // The plan mentions "min_edge_for_research" later.
            
            // Construct snapshot for LLM context
            let snapshot = json!({
                "yes_bid": market.yes_bid,
                "yes_ask": market.yes_ask,
                "no_bid": market.no_bid,
                "no_ask": market.no_ask,
                "last_price": market.last_price,
                "volume": market.volume,
                "open_interest": market.open_interest,
                "p_det_yes": deterministic.p_det_yes,
                "p_det_no": deterministic.p_det_no,
                "edge_yes_cents": deterministic.edge_yes_cents,
                "edge_no_cents": deterministic.edge_no_cents,
                "best_side": match deterministic.best_side {
                    Side::Yes => "yes",
                    Side::No => "no",
                },
                "best_edge_cents": deterministic.best_edge_cents,
                // Add more processed features if needed
            });

            candidates.push(ResearchCandidate {
                market,
                deterministic,
                complexity_score: score,
                complexity_reasons: reasons,
                snapshot,
            });
        }

        Ok(candidates)
    }
}
