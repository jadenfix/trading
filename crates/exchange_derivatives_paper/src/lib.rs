//! Paper-only derivatives adapter used for compliant rollout fallback.

use std::collections::HashMap;

use async_trait::async_trait;
use exchange_core::{ExchangeAdapter, ExchangeError};
use tokio::sync::RwLock;
use trading_domain::{
    AssetClass, BalanceSnapshot, ExecutionMode, InstrumentId, MarketType, OrderAck, OrderRequest,
    OrderSide, OrderStatus, OrderSummary, PositionSnapshot, VenueCapability, VenueHealth,
};
use uuid::Uuid;

#[derive(Debug, Default)]
struct AdapterState {
    positions: HashMap<String, PositionSnapshot>,
    orders: HashMap<String, OrderSummary>,
}

#[derive(Debug)]
pub struct DerivativesPaperAdapter {
    state: RwLock<AdapterState>,
}

impl Default for DerivativesPaperAdapter {
    fn default() -> Self {
        Self {
            state: RwLock::new(AdapterState::default()),
        }
    }
}

impl DerivativesPaperAdapter {
    fn validate_order(req: &OrderRequest) -> Result<(), ExchangeError> {
        if !matches!(
            req.instrument.market_type,
            MarketType::Perpetual | MarketType::Futures | MarketType::Option
        ) {
            return Err(ExchangeError {
                code: "unsupported_market_type".to_string(),
                message: "paper derivatives adapter only accepts derivative market types"
                    .to_string(),
                retriable: false,
            });
        }
        if req.quantity <= 0.0 {
            return Err(ExchangeError {
                code: "invalid_quantity".to_string(),
                message: "order quantity must be > 0".to_string(),
                retriable: false,
            });
        }
        Ok(())
    }
}

#[async_trait]
impl ExchangeAdapter for DerivativesPaperAdapter {
    fn venue(&self) -> &'static str {
        "derivatives_paper"
    }

    fn capabilities(&self) -> Vec<VenueCapability> {
        vec![
            VenueCapability {
                market_type: MarketType::Perpetual,
                supports_live: false,
                supports_paper: true,
                supports_post_only: true,
                supports_reduce_only: true,
            },
            VenueCapability {
                market_type: MarketType::Futures,
                supports_live: false,
                supports_paper: true,
                supports_post_only: true,
                supports_reduce_only: true,
            },
            VenueCapability {
                market_type: MarketType::Option,
                supports_live: false,
                supports_paper: true,
                supports_post_only: true,
                supports_reduce_only: true,
            },
        ]
    }

    fn execution_mode(&self) -> ExecutionMode {
        ExecutionMode::Paper
    }

    async fn connect_market_data(&self) -> Result<(), ExchangeError> {
        Ok(())
    }

    async fn place_order(&self, req: OrderRequest) -> Result<OrderAck, ExchangeError> {
        Self::validate_order(&req)?;

        let now = chrono::Utc::now().timestamp_millis();
        let venue_order_id = format!("deriv-paper-{}", Uuid::new_v4());

        let summary = OrderSummary {
            venue_order_id: venue_order_id.clone(),
            client_order_id: req.client_order_id.clone(),
            strategy_id: req.strategy_id.clone(),
            venue_id: req.venue_id.clone(),
            instrument: req.instrument.clone(),
            side: req.side,
            order_type: req.order_type,
            quantity: req.quantity,
            filled_quantity: req.quantity,
            avg_fill_price: req.limit_price,
            status: OrderStatus::Filled,
            created_at_ms: now,
            updated_at_ms: now,
            message: Some("paper fill".to_string()),
        };

        let mut state = self.state.write().await;
        let key = req.instrument.key();
        let signed = match req.side {
            OrderSide::Buy => req.quantity,
            OrderSide::Sell => -req.quantity,
        };
        let entry = state
            .positions
            .entry(key)
            .or_insert_with(|| PositionSnapshot {
                venue_id: req.venue_id.clone(),
                instrument: req.instrument.clone(),
                quantity: 0.0,
                avg_price: req.limit_price.unwrap_or(0.0),
                mark_price: req.limit_price,
                unrealized_pnl: Some(0.0),
            });
        entry.quantity += signed;
        if let Some(price) = req.limit_price {
            entry.avg_price = price;
            entry.mark_price = Some(price);
        }

        state.orders.insert(venue_order_id.clone(), summary);

        Ok(OrderAck {
            venue_order_id,
            accepted: true,
            status: OrderStatus::Filled,
            reason: None,
        })
    }

    async fn cancel_order(&self, venue_order_id: &str) -> Result<(), ExchangeError> {
        let mut state = self.state.write().await;
        let Some(order) = state.orders.get_mut(venue_order_id) else {
            return Err(ExchangeError {
                code: "order_not_found".to_string(),
                message: format!("unknown order id: {venue_order_id}"),
                retriable: false,
            });
        };
        order.status = OrderStatus::Canceled;
        order.updated_at_ms = chrono::Utc::now().timestamp_millis();
        Ok(())
    }

    async fn sync_positions(&self) -> Result<Vec<PositionSnapshot>, ExchangeError> {
        let state = self.state.read().await;
        Ok(state.positions.values().cloned().collect())
    }

    async fn sync_balances(&self) -> Result<Vec<BalanceSnapshot>, ExchangeError> {
        Ok(vec![BalanceSnapshot {
            venue_id: "derivatives_paper".to_string(),
            asset: "USD".to_string(),
            total: 20_000.0,
            available: 20_000.0,
        }])
    }

    async fn list_orders(&self) -> Result<Vec<OrderSummary>, ExchangeError> {
        let state = self.state.read().await;
        Ok(state.orders.values().cloned().collect())
    }

    fn health(&self) -> VenueHealth {
        VenueHealth {
            venue_id: "derivatives_paper".to_string(),
            healthy: true,
            connected_market_data: true,
            connected_trading: true,
            message: Some("paper-only derivatives venue".to_string()),
        }
    }
}

pub fn default_perp_instrument(symbol: &str) -> InstrumentId {
    InstrumentId {
        venue_id: "derivatives_paper".to_string(),
        symbol: symbol.to_string(),
        asset_class: AssetClass::Derivative,
        market_type: MarketType::Perpetual,
        expiry_ts_ms: None,
        strike: None,
        option_type: None,
        metadata: Default::default(),
    }
}
