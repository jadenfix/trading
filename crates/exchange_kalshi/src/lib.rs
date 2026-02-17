//! Kalshi adapter used by legacy workspace members.

use std::collections::HashMap;

use exchange_core::{
    AssetClass, BalanceSnapshot, ExchangeAdapter, ExchangeError, ExchangeHealth,
    ExchangeResultFuture, ExchangeValueFuture, FillReport, InstrumentRef, InstrumentType,
    NormalizedOrderRequest, OpenOrderSnapshot, OrderAck, OrderSide, OrderSnapshot, OrderStatus,
    PositionSnapshot,
};
use tokio::sync::RwLock;
use trading_domain::ExecutionMode;
use uuid::Uuid;

#[derive(Debug, Default)]
struct AdapterState {
    positions: HashMap<String, PositionSnapshot>,
    orders: HashMap<String, OrderSnapshot>,
    fills: Vec<FillReport>,
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

    fn now_ms() -> i64 {
        chrono::Utc::now().timestamp_millis()
    }

    fn validate_order(req: &NormalizedOrderRequest) -> Result<(), ExchangeError> {
        if req.instrument.instrument_type != InstrumentType::BinaryOption
            || req.instrument.asset_class != AssetClass::Prediction
        {
            return Err(ExchangeError::new(
                "unsupported_instrument",
                "kalshi adapter only accepts prediction binary-option instruments",
                false,
            ));
        }

        if req.qty <= 0.0 {
            return Err(ExchangeError::new(
                "invalid_quantity",
                "order qty must be > 0",
                false,
            ));
        }

        Ok(())
    }

    fn signed_qty(side: &OrderSide, qty: f64) -> f64 {
        match side {
            OrderSide::Buy => qty,
            OrderSide::Sell => -qty,
        }
    }

    fn mode_is_live(&self) -> bool {
        matches!(self.mode, ExecutionMode::Live)
    }

    fn fill_price(req: &NormalizedOrderRequest) -> f64 {
        req.limit_price.unwrap_or(0.5)
    }
}

impl ExchangeAdapter for KalshiAdapter {
    fn venue(&self) -> &'static str {
        "kalshi"
    }

    fn connect_market_data(&self) -> ExchangeResultFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }

    fn place_order(&self, req: NormalizedOrderRequest) -> ExchangeResultFuture<'_, OrderAck> {
        Box::pin(async move {
            Self::validate_order(&req)?;

            let now = Self::now_ms();
            let venue_order_id = format!("kalshi-{}", Uuid::new_v4());
            let status = if self.mode_is_live() {
                OrderStatus::New
            } else {
                OrderStatus::Filled
            };
            let filled_qty = if status == OrderStatus::Filled {
                req.qty
            } else {
                0.0
            };
            let avg_fill_price = if status == OrderStatus::Filled {
                Some(Self::fill_price(&req))
            } else {
                None
            };

            let snapshot = OrderSnapshot {
                venue: req.venue.clone(),
                venue_order_id: venue_order_id.clone(),
                client_order_id: req.client_order_id.clone(),
                strategy_id: req.strategy_id.clone(),
                instrument: req.instrument.clone(),
                side: req.side.clone(),
                order_type: req.order_type.clone(),
                status: status.clone(),
                qty: req.qty,
                filled_qty,
                limit_price: req.limit_price,
                avg_fill_price,
                created_at_ms: now,
                updated_at_ms: now,
                simulated: !self.mode_is_live(),
            };

            let mut state = self.state.write().await;
            if status == OrderStatus::Filled {
                let key = format!("{}:{}", req.instrument.venue, req.instrument.venue_symbol);
                let fill_price = Self::fill_price(&req);
                let entry = state
                    .positions
                    .entry(key)
                    .or_insert_with(|| PositionSnapshot {
                        venue: req.venue.clone(),
                        instrument: req.instrument.clone(),
                        qty: 0.0,
                        avg_price: fill_price,
                        mark_price: Some(fill_price),
                        unrealized_pnl: Some(0.0),
                    });
                entry.qty += Self::signed_qty(&req.side, req.qty);
                entry.avg_price = fill_price;
                entry.mark_price = Some(fill_price);

                state.fills.push(FillReport {
                    venue: req.venue.clone(),
                    venue_fill_id: format!("kalshi-fill-{}", Uuid::new_v4()),
                    venue_order_id: venue_order_id.clone(),
                    client_order_id: req.client_order_id.clone(),
                    strategy_id: req.strategy_id.clone(),
                    instrument: req.instrument.clone(),
                    side: req.side.clone(),
                    qty: req.qty,
                    price: fill_price,
                    fee: 0.0,
                    fee_asset: Some("USD".to_string()),
                    liquidity: Some("maker".to_string()),
                    simulated: true,
                    ts_ms: now,
                });
            }
            state.orders.insert(venue_order_id.clone(), snapshot);

            Ok(OrderAck {
                venue_order_id,
                client_order_id: req.client_order_id,
                accepted: true,
                status,
                filled_qty,
                avg_fill_price,
                simulated: !self.mode_is_live(),
                reason: None,
                ts_ms: now,
            })
        })
    }

    fn cancel_order(&self, venue_order_id: &str) -> ExchangeResultFuture<'_, ()> {
        let venue_order_id = venue_order_id.to_string();
        Box::pin(async move {
            let mut state = self.state.write().await;
            let order = state.orders.get_mut(&venue_order_id).ok_or_else(|| {
                ExchangeError::new(
                    "order_not_found",
                    format!("unknown order id: {venue_order_id}"),
                    false,
                )
            })?;
            order.status = OrderStatus::Canceled;
            order.updated_at_ms = Self::now_ms();
            Ok(())
        })
    }

    fn get_order(&self, venue_order_id: &str) -> ExchangeResultFuture<'_, Option<OrderSnapshot>> {
        let venue_order_id = venue_order_id.to_string();
        Box::pin(async move { Ok(self.state.read().await.orders.get(&venue_order_id).cloned()) })
    }

    fn open_orders(&self) -> ExchangeResultFuture<'_, Vec<OpenOrderSnapshot>> {
        Box::pin(async move {
            let state = self.state.read().await;
            Ok(state
                .orders
                .values()
                .filter(|order| matches!(order.status, OrderStatus::New | OrderStatus::PartiallyFilled))
                .cloned()
                .map(|order| OpenOrderSnapshot { order })
                .collect())
        })
    }

    fn fills_since(
        &self,
        since_ts_ms: i64,
        limit: usize,
    ) -> ExchangeResultFuture<'_, Vec<FillReport>> {
        Box::pin(async move {
            let state = self.state.read().await;
            let mut fills: Vec<FillReport> = state
                .fills
                .iter()
                .filter(|fill| fill.ts_ms >= since_ts_ms)
                .cloned()
                .collect();
            fills.sort_by_key(|fill| fill.ts_ms);
            if fills.len() > limit {
                fills = fills[fills.len().saturating_sub(limit)..].to_vec();
            }
            Ok(fills)
        })
    }

    fn sync_positions(&self) -> ExchangeResultFuture<'_, Vec<PositionSnapshot>> {
        Box::pin(async move {
            let state = self.state.read().await;
            Ok(state.positions.values().cloned().collect())
        })
    }

    fn sync_balances(&self) -> ExchangeResultFuture<'_, Vec<BalanceSnapshot>> {
        Box::pin(async move {
            let (total, available) = if self.mode_is_live() {
                (0.0, 0.0)
            } else {
                (100_000.0, 100_000.0)
            };
            Ok(vec![BalanceSnapshot {
                venue: self.venue().to_string(),
                asset: "USD".to_string(),
                total,
                available,
            }])
        })
    }

    fn health(&self) -> ExchangeValueFuture<'_, ExchangeHealth> {
        Box::pin(async move {
            ExchangeHealth {
                venue: self.venue().to_string(),
                healthy: true,
                connected_market_data: true,
                connected_trading: true,
                message: Some("adapter active".to_string()),
            }
        })
    }
}

pub fn default_kalshi_prediction_instrument(symbol: &str) -> InstrumentRef {
    InstrumentRef {
        venue: "kalshi".to_string(),
        venue_symbol: symbol.to_string(),
        asset_class: AssetClass::Prediction,
        instrument_type: InstrumentType::BinaryOption,
        base: None,
        quote: Some("USD".to_string()),
        expiry_ts_ms: None,
        strike: None,
        option_right: None,
        contract_multiplier: Some(1.0),
    }
}
