//! Paper-only derivatives adapter used by the legacy workspace members.

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
pub struct DerivativesPaperAdapter {
    mode: ExecutionMode,
    state: RwLock<AdapterState>,
}

impl Default for DerivativesPaperAdapter {
    fn default() -> Self {
        Self::new(ExecutionMode::Paper)
    }
}

impl DerivativesPaperAdapter {
    pub fn new(mode: ExecutionMode) -> Self {
        // This venue is intentionally paper-only.
        let mode = match mode {
            ExecutionMode::Paper => ExecutionMode::Paper,
            ExecutionMode::Live => ExecutionMode::Paper,
        };

        Self {
            mode,
            state: RwLock::new(AdapterState::default()),
        }
    }

    pub fn execution_mode(&self) -> ExecutionMode {
        self.mode
    }

    fn now_ms() -> i64 {
        chrono::Utc::now().timestamp_millis()
    }

    fn validate_order(req: &NormalizedOrderRequest) -> Result<(), ExchangeError> {
        if !matches!(
            req.instrument.instrument_type,
            InstrumentType::Perpetual | InstrumentType::Future | InstrumentType::Option
        ) {
            return Err(ExchangeError::new(
                "unsupported_instrument",
                "paper derivatives adapter only accepts perp/future/option instruments",
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

    fn fill_price(req: &NormalizedOrderRequest) -> f64 {
        req.limit_price.unwrap_or(1.0)
    }
}

impl ExchangeAdapter for DerivativesPaperAdapter {
    fn venue(&self) -> &'static str {
        "derivatives_paper"
    }

    fn connect_market_data(&self) -> ExchangeResultFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }

    fn place_order(&self, req: NormalizedOrderRequest) -> ExchangeResultFuture<'_, OrderAck> {
        Box::pin(async move {
            Self::validate_order(&req)?;

            let now = Self::now_ms();
            let venue_order_id = format!("deriv-paper-{}", Uuid::new_v4());
            let fill_price = Self::fill_price(&req);

            let snapshot = OrderSnapshot {
                venue: req.venue.clone(),
                venue_order_id: venue_order_id.clone(),
                client_order_id: req.client_order_id.clone(),
                strategy_id: req.strategy_id.clone(),
                instrument: req.instrument.clone(),
                side: req.side.clone(),
                order_type: req.order_type.clone(),
                status: OrderStatus::Filled,
                qty: req.qty,
                filled_qty: req.qty,
                limit_price: req.limit_price,
                avg_fill_price: Some(fill_price),
                created_at_ms: now,
                updated_at_ms: now,
                simulated: true,
            };

            let fill = FillReport {
                venue: req.venue.clone(),
                venue_fill_id: format!("deriv-fill-{}", Uuid::new_v4()),
                venue_order_id: venue_order_id.clone(),
                client_order_id: req.client_order_id.clone(),
                strategy_id: req.strategy_id.clone(),
                instrument: req.instrument.clone(),
                side: req.side.clone(),
                qty: req.qty,
                price: fill_price,
                fee: 0.0,
                fee_asset: req.instrument.quote.clone(),
                liquidity: Some("maker".to_string()),
                simulated: true,
                ts_ms: now,
            };

            let mut state = self.state.write().await;
            let key = format!("{}:{}", req.instrument.venue, req.instrument.venue_symbol);
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

            state.orders.insert(venue_order_id.clone(), snapshot);
            state.fills.push(fill);

            Ok(OrderAck {
                venue_order_id,
                client_order_id: req.client_order_id,
                accepted: true,
                status: OrderStatus::Filled,
                filled_qty: req.qty,
                avg_fill_price: Some(fill_price),
                simulated: true,
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
        Box::pin(async {
            Ok(vec![BalanceSnapshot {
                venue: "derivatives_paper".to_string(),
                asset: "USD".to_string(),
                total: 20_000.0,
                available: 20_000.0,
            }])
        })
    }

    fn health(&self) -> ExchangeValueFuture<'_, ExchangeHealth> {
        Box::pin(async {
            ExchangeHealth {
                venue: "derivatives_paper".to_string(),
                healthy: true,
                connected_market_data: true,
                connected_trading: true,
                message: Some("paper-only derivatives venue".to_string()),
            }
        })
    }
}

pub fn default_perp_instrument(symbol: &str) -> InstrumentRef {
    let (base, quote) = symbol
        .split_once('-')
        .map(|(b, q)| (Some(b.to_string()), Some(q.to_string())))
        .unwrap_or_else(|| (Some(symbol.to_string()), Some("USD".to_string())));

    InstrumentRef {
        venue: "derivatives_paper".to_string(),
        venue_symbol: symbol.to_string(),
        asset_class: AssetClass::Crypto,
        instrument_type: InstrumentType::Perpetual,
        base,
        quote,
        expiry_ts_ms: None,
        strike: None,
        option_right: None,
        contract_multiplier: Some(1.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_clamps_live_mode_to_paper() {
        let adapter = DerivativesPaperAdapter::new(ExecutionMode::Live);
        assert_eq!(adapter.execution_mode(), ExecutionMode::Paper);
    }

    #[test]
    fn adapter_default_mode_is_paper() {
        let adapter = DerivativesPaperAdapter::default();
        assert_eq!(adapter.execution_mode(), ExecutionMode::Paper);
    }
}
