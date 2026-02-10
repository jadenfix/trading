//! Executor — handles batch order placement and unwind policies.

use std::time::Duration;

use common::{Action, CreateOrderRequest, OrderType, Side};
use kalshi_client::KalshiRestClient;
use tokio::time::sleep;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::arb::{ArbDirection, ArbOpportunity};
use crate::config::ExecutionConfig;
use crate::fees::ArbFeeModel;

const CANCEL_RETRIES: usize = 3;
const UNWIND_RETRIES: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionOutcome {
    CompleteFill,
    NoFill,
    PartialFillUnwound,
    PartialFillUnwindFailed,
    Rejected,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnwindPolicy {
    CrossSpread,
    CancelOnly,
}

/// Executor engine.
pub struct ArbExecutor {
    client: KalshiRestClient,
    fees: ArbFeeModel,
    unwind_policy: UnwindPolicy,
}

impl ArbExecutor {
    pub fn new(
        client: &KalshiRestClient,
        fees: &ArbFeeModel,
        execution: &ExecutionConfig,
    ) -> Self {
        let unwind_policy = if execution.unwind_policy.eq_ignore_ascii_case("cancel_only") {
            UnwindPolicy::CancelOnly
        } else {
            UnwindPolicy::CrossSpread
        };

        Self {
            client: client.clone(),
            fees: fees.clone(),
            unwind_policy,
        }
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
                expiration_ts: None,
                time_in_force: Some("fill_or_kill".into()),
            });
        }

        // 2. Submit batch.
        info!("Sending batch of {} orders...", orders.len());

        match self.client.create_batch_orders(orders).await {
            Ok(responses) => {
                // 3. Unwrap BatchOrderEntry wrappers.
                //    Log per-order errors and extract successful OrderInfo objects.
                let mut successful_orders = Vec::new();
                let mut errored_count = 0usize;

                for entry in &responses.orders {
                    if let Some(ref err) = entry.error {
                        let coid = entry.client_order_id.as_deref().unwrap_or("?");
                        error!(
                            "Batch per-order error (client_order_id={}): code={:?} msg={:?}",
                            coid, err.code, err.message
                        );
                        errored_count += 1;
                        continue;
                    }
                    if let Some(ref order) = entry.order {
                        successful_orders.push(order.clone());
                    } else {
                        // Neither order nor error — treat as error.
                        warn!("Batch entry with no order and no error — skipping");
                        errored_count += 1;
                    }
                }

                if errored_count > 0 {
                    warn!(
                        "{} of {} batch entries had per-order errors",
                        errored_count,
                        responses.orders.len()
                    );
                }

                let total_legs = successful_orders.len();
                if total_legs == 0 && errored_count > 0 {
                    error!("All batch orders failed with per-order errors");
                    return ExecutionOutcome::Failed;
                }
                if total_legs == 0 {
                    error!("Batch returned zero order statuses");
                    return ExecutionOutcome::Failed;
                }

                let filled_orders: Vec<_> = successful_orders
                    .iter()
                    .filter(|o| o.status == "executed" || o.fill_count >= opp.qty)
                    .cloned()
                    .collect();
                let filled_legs = filled_orders.len();
                let open_orders: Vec<_> = successful_orders
                    .iter()
                    .filter(|o| {
                        o.remaining_count > 0 && !Self::is_terminal_status(o.status.as_str())
                    })
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
                    let unwound = match self.unwind_policy {
                        UnwindPolicy::CrossSpread => self.unwind_positions(filled_orders).await,
                        UnwindPolicy::CancelOnly => {
                            warn!(
                                "Unwind policy is cancel_only; leaving filled legs open for manual reconciliation"
                            );
                            false
                        }
                    };
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

            let mut canceled = false;
            for attempt in 1..=CANCEL_RETRIES {
                match self.client.cancel_order(&order.order_id).await {
                    Ok(()) => {
                        warn!(
                            "Canceled residual order {} (ticker={} status={} remaining={})",
                            order.order_id, order.ticker, order.status, order.remaining_count
                        );
                        canceled = true;
                        break;
                    }
                    Err(e) if attempt < CANCEL_RETRIES => {
                        warn!(
                            "Cancel retry {}/{} failed for {} (ticker={}): {}",
                            attempt, CANCEL_RETRIES, order.order_id, order.ticker, e
                        );
                        sleep(Duration::from_millis(100)).await;
                    }
                    Err(e) => {
                        all_ok = false;
                        error!(
                            "Failed to cancel residual order {} after {} attempts (ticker={}): {}",
                            order.order_id, CANCEL_RETRIES, order.ticker, e
                        );
                    }
                }
            }
            if !canceled {
                all_ok = false;
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

            let mut unwound = false;
            for attempt in 1..=UNWIND_RETRIES {
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
                    time_in_force: None, // Market orders don't need TIF
                };

                // Use batch endpoint so we can pass true Market order payload.
                match self.client.create_batch_orders(vec![req.clone()]).await {
                    Ok(resp) => {
                        let first_order = resp.orders.first()
                            .and_then(|entry| entry.order.as_ref());
                        let status = first_order.map(|o| o.status.as_str()).unwrap_or("unknown");
                        let fill_count = first_order.map(|o| o.fill_count).unwrap_or(0);
                        if fill_count >= req.count || status == "executed" {
                            warn!("Unwound {} x {} (status={})", req.ticker, req.count, status);
                            unwound = true;
                            break;
                        }
                        warn!(
                            "Unwind retry {}/{} for {} returned status={} fill_count={}/{}",
                            attempt, UNWIND_RETRIES, req.ticker, status, fill_count, req.count
                        );
                    }
                    Err(e) if attempt < UNWIND_RETRIES => {
                        warn!(
                            "Unwind retry {}/{} failed for {}: {}",
                            attempt, UNWIND_RETRIES, order.ticker, e
                        );
                    }
                    Err(e) => {
                        error!(
                            "CRITICAL: FAILED TO UNWIND {} after {} attempts: {}",
                            order.ticker, UNWIND_RETRIES, e
                        );
                    }
                }

                if attempt < UNWIND_RETRIES {
                    sleep(Duration::from_millis(150)).await;
                }
            }

            if !unwound {
                all_ok = false;
            }

            // Small delay to avoid rate limits during panic
            sleep(Duration::from_millis(50)).await;
        }
        all_ok
    }
}
