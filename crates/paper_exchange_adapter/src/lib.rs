use std::collections::HashMap;

use chrono::Utc;
use exchange_core::{
    BalanceSnapshot, ExchangeAdapter, ExchangeError, ExchangeHealth, ExchangeResultFuture,
    ExchangeValueFuture, FillReport, InstrumentType, NormalizedOrderRequest, OpenOrderSnapshot,
    OptionRight, OrderAck, OrderSide, OrderSnapshot, OrderStatus, PositionSnapshot,
};
use tokio::sync::Mutex;
use uuid::Uuid;

struct PaperState {
    orders: HashMap<String, OrderSnapshot>,
    fills: Vec<FillReport>,
    positions: HashMap<String, PositionSnapshot>,
    balances: HashMap<String, BalanceSnapshot>,
}

impl PaperState {
    fn new(venue: &str) -> Self {
        let mut balances = HashMap::new();
        balances.insert(
            "USD".to_string(),
            BalanceSnapshot {
                venue: venue.to_string(),
                asset: "USD".to_string(),
                total: 1_000_000.0,
                available: 1_000_000.0,
            },
        );

        Self {
            orders: HashMap::new(),
            fills: Vec::new(),
            positions: HashMap::new(),
            balances,
        }
    }
}

pub struct PaperExchangeAdapter {
    venue: String,
    state: Mutex<PaperState>,
}

impl PaperExchangeAdapter {
    pub fn new(venue: impl Into<String>) -> Self {
        let venue = venue.into();
        Self {
            state: Mutex::new(PaperState::new(&venue)),
            venue,
        }
    }

    fn now_ms() -> i64 {
        Utc::now().timestamp_millis()
    }

    fn deterministic_mark_price(symbol: &str) -> f64 {
        let hash = symbol
            .bytes()
            .fold(0_u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64));
        let base = 10.0 + (hash % 50_000) as f64 / 100.0;
        (base * 100.0).round() / 100.0
    }

    fn slippage_bps(instrument_type: &InstrumentType, option_right: &Option<OptionRight>) -> f64 {
        match instrument_type {
            InstrumentType::Spot => 5.0,
            InstrumentType::Perpetual | InstrumentType::Future => 8.0,
            InstrumentType::Option | InstrumentType::BinaryOption => {
                if option_right.is_some() {
                    25.0
                } else {
                    20.0
                }
            }
            InstrumentType::Custom => 15.0,
        }
    }

    fn quote_asset(req: &NormalizedOrderRequest) -> String {
        req.instrument
            .quote
            .clone()
            .unwrap_or_else(|| "USD".to_string())
    }

    fn base_asset(req: &NormalizedOrderRequest) -> String {
        req.instrument
            .base
            .clone()
            .unwrap_or_else(|| req.symbol.clone())
    }

    fn apply_balance_delta(state: &mut PaperState, venue: &str, asset: &str, delta: f64) {
        let entry = state
            .balances
            .entry(asset.to_string())
            .or_insert_with(|| BalanceSnapshot {
                venue: venue.to_string(),
                asset: asset.to_string(),
                total: 0.0,
                available: 0.0,
            });

        entry.total += delta;
        entry.available += delta;
    }

    fn update_position_after_fill(
        position: &mut PositionSnapshot,
        side: &OrderSide,
        qty: f64,
        fill_price: f64,
    ) {
        let signed_delta = if *side == OrderSide::Buy { qty } else { -qty };
        let old_qty = position.qty;
        let new_qty = old_qty + signed_delta;
        let old_abs = old_qty.abs();
        let delta_abs = signed_delta.abs();
        let new_abs = new_qty.abs();
        let epsilon = f64::EPSILON;

        if old_abs <= epsilon {
            position.avg_price = fill_price;
        } else if old_qty.is_sign_positive() == signed_delta.is_sign_positive() {
            if new_abs > epsilon {
                position.avg_price =
                    ((position.avg_price * old_abs) + (fill_price * delta_abs)) / new_abs;
            } else {
                position.avg_price = fill_price;
            }
        } else if delta_abs > old_abs + epsilon || new_abs <= epsilon {
            // Trade crossed through zero (or flattened), so remaining basis is this fill.
            position.avg_price = fill_price;
        }

        position.qty = if new_abs <= epsilon { 0.0 } else { new_qty };
        position.mark_price = Some(fill_price);
    }
}

impl ExchangeAdapter for PaperExchangeAdapter {
    fn venue(&self) -> &'static str {
        "paper"
    }

    fn connect_market_data(&self) -> ExchangeResultFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }

    fn place_order(&self, req: NormalizedOrderRequest) -> ExchangeResultFuture<'_, OrderAck> {
        Box::pin(async move {
            let mark = req
                .limit_price
                .unwrap_or_else(|| Self::deterministic_mark_price(&req.instrument.venue_symbol));
            let slippage = Self::slippage_bps(
                &req.instrument.instrument_type,
                &req.instrument.option_right,
            ) / 10_000.0;
            let fill_price = match req.side {
                OrderSide::Buy => mark * (1.0 + slippage),
                OrderSide::Sell => mark * (1.0 - slippage),
            };
            let now = Self::now_ms();
            let venue_order_id = format!("paper-{}", Uuid::new_v4().as_simple());

            let order = OrderSnapshot {
                venue: self.venue.clone(),
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
                venue: self.venue.clone(),
                venue_fill_id: format!("fill-{}", Uuid::new_v4().as_simple()),
                venue_order_id: venue_order_id.clone(),
                client_order_id: req.client_order_id.clone(),
                strategy_id: req.strategy_id.clone(),
                instrument: req.instrument.clone(),
                side: req.side.clone(),
                qty: req.qty,
                price: fill_price,
                fee: (req.qty * fill_price * 0.0005).max(0.0),
                fee_asset: Some(Self::quote_asset(&req)),
                liquidity: Some("taker".to_string()),
                simulated: true,
                ts_ms: now,
            };

            let mut state = self.state.lock().await;
            state.orders.insert(venue_order_id.clone(), order);
            state.fills.push(fill);

            let quote = Self::quote_asset(&req);
            let base = Self::base_asset(&req);
            let notional = req.qty * fill_price;

            match req.side {
                OrderSide::Buy => {
                    Self::apply_balance_delta(&mut state, &self.venue, &quote, -notional);
                    Self::apply_balance_delta(&mut state, &self.venue, &base, req.qty);
                }
                OrderSide::Sell => {
                    Self::apply_balance_delta(&mut state, &self.venue, &quote, notional);
                    Self::apply_balance_delta(&mut state, &self.venue, &base, -req.qty);
                }
            }

            let position_key = format!("{}:{}", req.instrument.venue, req.instrument.venue_symbol);
            let position =
                state
                    .positions
                    .entry(position_key)
                    .or_insert_with(|| PositionSnapshot {
                        venue: self.venue.clone(),
                        instrument: req.instrument.clone(),
                        qty: 0.0,
                        avg_price: fill_price,
                        mark_price: Some(fill_price),
                        unrealized_pnl: Some(0.0),
                    });

            Self::update_position_after_fill(position, &req.side, req.qty, fill_price);


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
            let mut state = self.state.lock().await;
            let order = state.orders.get_mut(&venue_order_id).ok_or_else(|| {
                ExchangeError::new(
                    "order_not_found",
                    format!("unknown order id: {}", venue_order_id),
                    false,
                )
            })?;

            if matches!(order.status, OrderStatus::Filled | OrderStatus::Rejected) {
                return Ok(());
            }

            order.status = OrderStatus::Canceled;
            order.updated_at_ms = Self::now_ms();
            Ok(())
        })
    }

    fn get_order(&self, venue_order_id: &str) -> ExchangeResultFuture<'_, Option<OrderSnapshot>> {
        let venue_order_id = venue_order_id.to_string();
        Box::pin(async move { Ok(self.state.lock().await.orders.get(&venue_order_id).cloned()) })
    }

    fn open_orders(&self) -> ExchangeResultFuture<'_, Vec<OpenOrderSnapshot>> {
        Box::pin(async move {
            let state = self.state.lock().await;
            Ok(state
                .orders
                .values()
                .filter(|order| {
                    matches!(
                        order.status,
                        OrderStatus::New | OrderStatus::PartiallyFilled
                    )
                })
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
            let state = self.state.lock().await;
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
            let state = self.state.lock().await;
            Ok(state.positions.values().cloned().collect())
        })
    }

    fn sync_balances(&self) -> ExchangeResultFuture<'_, Vec<BalanceSnapshot>> {
        Box::pin(async move {
            let state = self.state.lock().await;
            Ok(state.balances.values().cloned().collect())
        })
    }

    fn health(&self) -> ExchangeValueFuture<'_, ExchangeHealth> {
        Box::pin(async move {
            ExchangeHealth {
                venue: self.venue.clone(),
                healthy: true,
                connected_market_data: true,
                connected_trading: true,
                message: Some("paper adapter ready".to_string()),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use exchange_core::{
        InstrumentRef, InstrumentType, NormalizedOrderRequest, OrderSide, OrderStatus, OrderType,
        TimeInForce,
    };

    use super::*;

    fn spot_order(
        client_order_id: &str,
        side: OrderSide,
        qty: f64,
        limit_price: f64,
    ) -> NormalizedOrderRequest {
        NormalizedOrderRequest {
            venue: "paper".to_string(),
            symbol: "BTC-USD".to_string(),
            instrument: InstrumentRef {
                venue: "paper".to_string(),
                venue_symbol: "BTC-USD".to_string(),
                asset_class: exchange_core::AssetClass::Crypto,
                instrument_type: InstrumentType::Spot,
                base: Some("BTC".to_string()),
                quote: Some("USD".to_string()),
                expiry_ts_ms: None,
                strike: None,
                option_right: None,
                contract_multiplier: Some(1.0),
            },
            strategy_id: "test.strategy".to_string(),
            client_order_id: client_order_id.to_string(),
            intent_id: Some(format!("intent-{client_order_id}")),
            side,
            order_type: OrderType::Limit,
            qty,
            limit_price: Some(limit_price),
            tif: Some(TimeInForce::Gtc),
            post_only: false,
            reduce_only: false,
            requested_notional_cents: (qty * limit_price * 100.0) as i64,
        }
    }

    #[tokio::test]
    async fn fills_spot_order_and_updates_balances() {
        let adapter = PaperExchangeAdapter::new("paper");
        let order = spot_order("client-1", OrderSide::Buy, 1.0, 100.0);

        let ack = adapter.place_order(order).await.expect("order should fill");
        assert!(ack.accepted);
        assert_eq!(ack.status, OrderStatus::Filled);

        let balances = adapter.sync_balances().await.expect("balances");
        assert!(balances.iter().any(|b| b.asset == "BTC" && b.total > 0.0));
    }

    #[tokio::test]
    async fn sell_reduction_keeps_existing_long_basis() {
        let adapter = PaperExchangeAdapter::new("paper");
        adapter
            .place_order(spot_order("client-1", OrderSide::Buy, 2.0, 100.0))
            .await
            .expect("buy should fill");
        adapter
            .place_order(spot_order("client-2", OrderSide::Sell, 1.0, 120.0))
            .await
            .expect("sell should fill");

        let positions = adapter.sync_positions().await.expect("positions");
        let position = positions
            .iter()
            .find(|p| p.instrument.venue_symbol == "BTC-USD")
            .expect("position should exist");
        assert!((position.qty - 1.0).abs() < 1e-9);
        assert!((position.avg_price - 100.05).abs() < 1e-9);
    }

    #[tokio::test]
    async fn side_flip_resets_basis_to_flip_fill_price() {
        let adapter = PaperExchangeAdapter::new("paper");
        adapter
            .place_order(spot_order("client-1", OrderSide::Buy, 1.0, 100.0))
            .await
            .expect("buy should fill");
        adapter
            .place_order(spot_order("client-2", OrderSide::Sell, 2.0, 120.0))
            .await
            .expect("sell should fill");

        let positions = adapter.sync_positions().await.expect("positions");
        let position = positions
            .iter()
            .find(|p| p.instrument.venue_symbol == "BTC-USD")
            .expect("position should exist");
        assert!((position.qty + 1.0).abs() < 1e-9);
        assert!((position.avg_price - 119.94).abs() < 1e-9);
    }

    #[tokio::test]
    async fn short_addition_recomputes_weighted_basis() {
        let adapter = PaperExchangeAdapter::new("paper");
        adapter
            .place_order(spot_order("client-1", OrderSide::Sell, 1.0, 100.0))
            .await
            .expect("sell should fill");
        adapter
            .place_order(spot_order("client-2", OrderSide::Sell, 1.0, 110.0))
            .await
            .expect("sell should fill");

        let positions = adapter.sync_positions().await.expect("positions");
        let position = positions
            .iter()
            .find(|p| p.instrument.venue_symbol == "BTC-USD")
            .expect("position should exist");
        assert!((position.qty + 2.0).abs() < 1e-9);
        assert!((position.avg_price - 104.9475).abs() < 1e-9);
    }
}
