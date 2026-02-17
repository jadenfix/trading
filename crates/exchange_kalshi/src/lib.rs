//! Kalshi exchange adapter for the unified trading runtime.

use std::collections::HashMap;

use async_trait::async_trait;
use exchange_core::{ExchangeAdapter, ExchangeError};
use tokio::sync::RwLock;
use trading_domain::{
    BalanceSnapshot, ExecutionMode, InstrumentId, MarketType, OrderAck, OrderRequest, OrderSide,
    OrderStatus, OrderSummary, PositionSnapshot, VenueCapability, VenueHealth,
};
use uuid::Uuid;

#[derive(Debug, Default)]
struct AdapterState {
    positions: HashMap<String, PositionSnapshot>,
    orders: HashMap<String, OrderSummary>,
}

#[derive(Debug)]
pub struct KalshiAdapter {
    mode: ExecutionMode,
    state: RwLock<AdapterState>,
}

impl KalshiAdapter {
    pub fn new(mode: ExecutionMode) -> Self {
        Self {
            mode,
            state: RwLock::new(AdapterState::default()),
        }
    }

    fn validate_order(req: &OrderRequest) -> Result<(), ExchangeError> {
        if req.instrument.market_type != MarketType::Binary {
            return Err(ExchangeError {
                code: "unsupported_market_type".to_string(),
                message: "Kalshi adapter only accepts binary instruments".to_string(),
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

    fn signed_qty(side: OrderSide, qty: f64) -> f64 {
        match side {
            OrderSide::Buy => qty,
            OrderSide::Sell => -qty,
        }
    }
}

#[async_trait]
impl ExchangeAdapter for KalshiAdapter {
    fn venue(&self) -> &'static str {
        "kalshi"
    }

    fn capabilities(&self) -> Vec<VenueCapability> {
        vec![VenueCapability {
            market_type: MarketType::Binary,
            supports_live: true,
            supports_paper: true,
            supports_post_only: false,
            supports_reduce_only: true,
        }]
    }

    fn execution_mode(&self) -> ExecutionMode {
        self.mode
    }

    async fn connect_market_data(&self) -> Result<(), ExchangeError> {
        Ok(())
    }

    async fn place_order(&self, req: OrderRequest) -> Result<OrderAck, ExchangeError> {
        Self::validate_order(&req)?;

        let now = chrono::Utc::now().timestamp_millis();
        let venue_order_id = format!("kalshi-{}", Uuid::new_v4());
        let status = match self.mode {
            ExecutionMode::Paper => OrderStatus::Filled,
            ExecutionMode::Live => OrderStatus::Accepted,
        };

        let summary = OrderSummary {
            venue_order_id: venue_order_id.clone(),
            client_order_id: req.client_order_id.clone(),
            strategy_id: req.strategy_id.clone(),
            venue_id: req.venue_id.clone(),
            instrument: req.instrument.clone(),
            side: req.side,
            order_type: req.order_type,
            quantity: req.quantity,
            filled_quantity: if status == OrderStatus::Filled {
                req.quantity
            } else {
                0.0
            },
            avg_fill_price: req.limit_price,
            status,
            created_at_ms: now,
            updated_at_ms: now,
            message: None,
        };

        let mut state = self.state.write().await;
        if status == OrderStatus::Filled {
            let key = req.instrument.key();
            let delta = Self::signed_qty(req.side, req.quantity);
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
            entry.quantity += delta;
            if let Some(price) = req.limit_price {
                entry.avg_price = price;
                entry.mark_price = Some(price);
            }
        }
        state.orders.insert(venue_order_id.clone(), summary);

        Ok(OrderAck {
            venue_order_id,
            accepted: true,
            status,
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
        order.message = Some("canceled by operator".to_string());
        Ok(())
    }

    async fn sync_positions(&self) -> Result<Vec<PositionSnapshot>, ExchangeError> {
        let state = self.state.read().await;
        Ok(state.positions.values().cloned().collect())
    }

    async fn sync_balances(&self) -> Result<Vec<BalanceSnapshot>, ExchangeError> {
        let (total, available) = match self.mode {
            ExecutionMode::Paper => (100_000.0, 100_000.0),
            ExecutionMode::Live => (0.0, 0.0),
        };
        Ok(vec![BalanceSnapshot {
            venue_id: "kalshi".to_string(),
            asset: "USD".to_string(),
            total,
            available,
        }])
    }

    async fn list_orders(&self) -> Result<Vec<OrderSummary>, ExchangeError> {
        let state = self.state.read().await;
        Ok(state.orders.values().cloned().collect())
    }

    fn health(&self) -> VenueHealth {
        VenueHealth {
            venue_id: "kalshi".to_string(),
            healthy: true,
            connected_market_data: true,
            connected_trading: true,
            message: Some("adapter active".to_string()),
        }
    }
}

pub fn default_kalshi_prediction_instrument(symbol: &str) -> InstrumentId {
    InstrumentId {
        venue_id: "kalshi".to_string(),
        symbol: symbol.to_string(),
        asset_class: trading_domain::AssetClass::Prediction,
        market_type: MarketType::Binary,
        expiry_ts_ms: None,
        strike: None,
        option_type: None,
        metadata: Default::default(),
    }
}
