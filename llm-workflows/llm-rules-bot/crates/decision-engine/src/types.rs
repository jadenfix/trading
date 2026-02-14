use common::{Action, Side};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Decision {
    Approve(TradeIntent),
    Veto(VetoReason),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeIntent {
    pub ticker: String,
    pub side: Side,
    pub action: Action,
    pub price_cents: i64,
    pub size_contracts: i64,
    pub edge_cents: f64,
    pub confidence: f64,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VetoReason {
    pub ticker: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeterministicInput {
    pub ticker: String,
    pub target_side: Side,
    pub target_price_cents: i64,
    pub p_det_target: f64,
    pub best_edge_cents: f64,
}
