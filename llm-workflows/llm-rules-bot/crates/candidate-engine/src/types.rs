use common::MarketInfo;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchCandidate {
    pub market: MarketInfo,
    pub deterministic: DeterministicSignal,
    pub complexity_score: f64,
    pub complexity_reasons: Vec<String>,
    pub snapshot: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeterministicSignal {
    pub p_det_yes: f64,
    pub p_det_no: f64,
    pub edge_yes_cents: f64,
    pub edge_no_cents: f64,
    pub best_side: common::Side,
    pub best_price_cents: i64,
    pub best_edge_cents: f64,
}
