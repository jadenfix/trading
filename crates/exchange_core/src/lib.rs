//! Exchange abstraction layer for normalized, venue-agnostic execution.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use trading_domain::{
    BalanceSnapshot, ExecutionMode, OrderAck, OrderRequest, OrderSummary, PositionSnapshot,
    VenueCapability, VenueHealth,
};

pub type NormalizedOrderRequest = OrderRequest;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExchangeError {
    pub code: String,
    pub message: String,
    pub retriable: bool,
}

#[async_trait]
pub trait ExchangeAdapter: Send + Sync {
    fn venue(&self) -> &'static str;

    fn capabilities(&self) -> Vec<VenueCapability>;

    fn execution_mode(&self) -> ExecutionMode;

    async fn connect_market_data(&self) -> Result<(), ExchangeError>;

    async fn place_order(&self, req: NormalizedOrderRequest) -> Result<OrderAck, ExchangeError>;

    async fn cancel_order(&self, venue_order_id: &str) -> Result<(), ExchangeError>;

    async fn sync_positions(&self) -> Result<Vec<PositionSnapshot>, ExchangeError>;

    async fn sync_balances(&self) -> Result<Vec<BalanceSnapshot>, ExchangeError>;

    async fn list_orders(&self) -> Result<Vec<OrderSummary>, ExchangeError>;

    fn health(&self) -> VenueHealth;
}
