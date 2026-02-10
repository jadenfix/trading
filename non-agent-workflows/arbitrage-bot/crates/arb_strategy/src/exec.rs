//! Executor — handles batch order placement and unwind policies.

use std::time::Duration;

use common::{Action, CreateOrderRequest, OrderType, Side};
use kalshi_client::KalshiRestClient;
use tokio::time::sleep;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::arb::{ArbDirection, ArbOpportunity};
use crate::fees::ArbFeeModel;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionOutcome {
    CompleteFill,
    NoFill,
    PartialFillUnwound,
    PartialFillUnwindFailed,
    Rejected,
    Failed,
}

/// Executor engine.
pub struct ArbExecutor<'a> {
    client: &'a KalshiRestClient,
    fees: &'a ArbFeeModel,
}

impl<'a> ArbExecutor<'a> {
    pub fn new(client: &'a KalshiRestClient, fees: &'a ArbFeeModel) -> Self {
        Self { client, fees }
    }

    /// Execute an arb opportunity using a batched FOK order.
    ///
    /// If partial fills occur (despite FOK intention, batch is not atomic),
    /// this will trigger the unwind policy.
    pub async fn execute(&self, opp: &ArbOpportunity) -> ExecutionOutcome {
        info!("EXECUTING: {}", opp.reason);

        if opp.qty <= 0 || opp.legs.is_empty() {
            warn!(
                "{}: invalid opportunity payload (qty={}, legs={})",
                opp.group_event_ticker,
                opp.qty,
                opp.legs.len()
            );
            return ExecutionOutcome::Rejected;
        }

        // 1. Build batch order requests.
        let mut orders = Vec::new();

        for leg in &opp.legs {
            // Apply slippage buffer to limit price.
            let limit_price = match opp.direction {
                ArbDirection::BuySet => leg.price_cents + self.fees.slippage_buffer,
                ArbDirection::SellSet => leg.price_cents - self.fees.slippage_buffer,
            };

            if !(1..=99).contains(&limit_price) {
                warn!(
                    "{}: invalid executable limit {} for {} (raw={} slippage={} direction={:?}); rejecting",
                    opp.group_event_ticker,
                    limit_price,
                    leg.ticker,
                    leg.price_cents,
                    self.fees.slippage_buffer,
                    opp.direction
                );
                return ExecutionOutcome::Rejected;
            }

            let action = match opp.direction {
                ArbDirection::BuySet => Action::Buy,
                ArbDirection::SellSet => Action::Sell,
            };

            let (yes_price, no_price) = match leg.side {
                Side::Yes => (Some(limit_price), None),
                Side::No => (None, Some(limit_price)),
            };

            orders.push(CreateOrderRequest {
                ticker: leg.ticker.clone(),
                side: leg.side,
                action,
                client_order_id: Uuid::new_v4().to_string(),
                count: opp.qty,
                order_type: OrderType::Limit,
                yes_price,
                no_price,
                // TODO: Set expiration_ts for strict FOK if API supports it,
                // currently using FOK simply by immediate cancel or reliance on fill.
                // Kalshi "FOK" is a TimeInForce param but CreateOrderRequest struct
                // in common might need update or we use correct parameter.
                // For now, we use standard Limit orders and check fills immediately.
                expiration_ts: None,
            });
        }

        // 2. Submit batch.
        // Note: The common crate CreateOrderRequest doesn't have `expiration_ts` nicely typed for FOK.
        // We will assume `create_batch_orders` handles the raw request.
        info!("Sending batch of {} orders...", orders.len());

        // This assumes KalshiRestClient has a `create_batch_orders` method (added in plan).
        match self.client.create_batch_orders(orders).await {
            Ok(responses) => {
                // 3. Analyze fills.
                let total_legs = responses.orders.len();
                if total_legs == 0 {
                    error!("Batch returned zero order statuses");
                    return ExecutionOutcome::Failed;
                }

                let filled_orders: Vec<_> = responses
                    .orders
                    .iter()
                    .filter(|o| o.status == "executed" || o.fill_count == opp.qty)
                    .cloned()
                    .collect();
                let filled_legs = filled_orders.len();
                let open_orders: Vec<_> = responses
                    .orders
                    .iter()
                    .filter(|o| o.fill_count < opp.qty)
                    .cloned()
                    .collect();

                if filled_legs == total_legs {
                    info!(
                        "✅ COMPLETE FILL: all {} legs executed for {}¢ profit!",
                        total_legs, opp.net_edge_cents
                    );
                    ExecutionOutcome::CompleteFill
                } else if filled_legs == 0 {
                    // Best-effort cleanup: batch orders are not atomic and may rest.
                    let canceled = self.cancel_open_orders(&open_orders).await;
                    if !canceled {
                        warn!("No-fill batch had uncanceled resting orders");
                    }
                    info!("❌ NO FILLS: batch failed (likely price moved). No risk.");
                    ExecutionOutcome::NoFill
                } else {
                    // PARTIAL FILL SCENARIO - DANGER!
                    error!(
                        "⚠️ PARTIAL FILL: {}/{} legs filled. UNWINDING IMMEDIATELY!",
                        filled_legs, total_legs
                    );

                    // Cancel unfilled residuals first, then flatten fills.
                    let canceled = self.cancel_open_orders(&open_orders).await;
                    let unwound = self.unwind_positions(filled_orders).await;
                    if canceled && unwound {
                        ExecutionOutcome::PartialFillUnwound
                    } else {
                        ExecutionOutcome::PartialFillUnwindFailed
                    }
                }
            }
            Err(e) => {
                error!("Batch order submission failed: {}", e);
                ExecutionOutcome::Failed
            }
        }
    }

    fn is_terminal_status(status: &str) -> bool {
        matches!(
            status,
            "executed" | "canceled" | "cancelled" | "expired" | "rejected"
        )
    }

    async fn cancel_open_orders(&self, orders: &[common::OrderInfo]) -> bool {
        let mut all_ok = true;
        for order in orders {
            if order.order_id.is_empty() {
                continue;
            }
            if order.remaining_count <= 0 || Self::is_terminal_status(order.status.as_str()) {
                continue;
            }
            match self.client.cancel_order(&order.order_id).await {
                Ok(()) => {
                    warn!(
                        "Canceled residual order {} (ticker={} status={} remaining={})",
                        order.order_id, order.ticker, order.status, order.remaining_count
                    );
                }
                Err(e) => {
                    all_ok = false;
                    error!(
                        "Failed to cancel residual order {} (ticker={}): {}",
                        order.order_id, order.ticker, e
                    );
                }
            }
        }
        all_ok
    }

    /// Emergency unwind of partial fills.
    async fn unwind_positions(&self, orders: Vec<common::OrderInfo>) -> bool {
        let mut all_ok = true;
        for order in orders {
            if order.fill_count <= 0 {
                continue;
            }

            let unwind_action = match order.action {
                Action::Buy => Action::Sell,
                Action::Sell => Action::Buy,
            };

            let req = CreateOrderRequest {
                ticker: order.ticker.clone(),
                side: order.side,
                action: unwind_action,
                client_order_id: Uuid::new_v4().to_string(),
                count: order.fill_count,
                order_type: OrderType::Market,
                yes_price: None,
                no_price: None,
                expiration_ts: None,
            };

            // Use batch endpoint so we can pass true Market order payload.
            match self.client.create_batch_orders(vec![req.clone()]).await {
                Ok(resp) => {
                    let status = resp
                        .orders
                        .first()
                        .map(|o| o.status.as_str())
                        .unwrap_or("unknown");
                    warn!("Unwound {} x {} (status={})", req.ticker, req.count, status);
                }
                Err(e) => {
                    all_ok = false;
                    error!("CRITICAL: FAILED TO UNWIND {}: {}", req.ticker, e);
                    // Retry logic would go here.
                }
            }
            // Small delay to avoid rate limits during panic
            sleep(Duration::from_millis(50)).await;
        }
        all_ok
    }
}
