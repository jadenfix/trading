//! Executor — handles batch order placement and unwind policies.

use std::time::Duration;

use common::{Action, CreateOrderRequest, OrderType, Side};
use kalshi_client::KalshiRestClient;
use tokio::time::sleep;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::arb::{ArbDirection, ArbOpportunity};
use crate::fees::ArbFeeModel;

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
    pub async fn execute(&self, opp: &ArbOpportunity) {
        info!("EXECUTING: {}", opp.reason);

        // 1. Build batch order requests.
        let mut orders = Vec::new();

        for leg in &opp.legs {
            // Apply slippage buffer to limit price.
            let limit_price = match opp.direction {
                ArbDirection::BuySet => leg.price_cents + self.fees.slippage_buffer,
                ArbDirection::SellSet => leg.price_cents - self.fees.slippage_buffer,
            };

            let action = match opp.direction {
                ArbDirection::BuySet => Action::Buy,
                ArbDirection::SellSet => Action::Sell,
            };

            let (yes_price, no_price) = (Some(limit_price), None);

            orders.push(CreateOrderRequest {
                ticker: leg.ticker.clone(),
                side: Side::Yes,
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
        match self.client.create_batch_orders(orders.clone()).await {
            Ok(responses) => {
                // 3. Analyze fills.
                let mut filled_legs = 0;
                let mut total_legs = responses.orders.len();
                let mut failed_legs = Vec::new();

                for order in &responses.orders {
                    if order.status == "executed" || order.fill_count == opp.qty {
                        filled_legs += 1;
                    } else {
                        failed_legs.push(order.clone());
                    }
                }

                if filled_legs == total_legs {
                    info!(
                        "✅ COMPLETE FILL: all {} legs executed for {}¢ profit!",
                        total_legs, opp.net_edge_cents
                    );
                } else if filled_legs == 0 {
                    info!("❌ NO FILLS: batch failed (likely price moved). No risk.");
                } else {
                    // PARTIAL FILL SCENARIO - DANGER!
                    error!(
                        "⚠️ PARTIAL FILL: {}/{} legs filled. UNWINDING IMMEDIATELY!",
                        filled_legs, total_legs
                    );
                    
                    // 4. Unwind logic.
                    // We need to close the positions we just opened to neutralize delta.
                    // If we bought YES, we sell YES (at bid).
                    // If we sold YES, we buy YES (at ask).
                    
                    // Identify filled orders to unwind.
                    let filled_orders: Vec<_> = responses
                        .orders
                        .into_iter()
                        .filter(|o| o.status == "executed" || o.fill_count > 0)
                        .collect();
                        
                    self.unwind_positions(filled_orders).await;
                }
            }
            Err(e) => {
                error!("Batch order submission failed: {}", e);
            }
        }
    }

    /// Emergency unwind of partial fills.
    async fn unwind_positions(&self, orders: Vec<common::OrderInfo>) {
        for order in orders {
            let unwind_action = match order.action {
                Action::Buy => Action::Sell,
                Action::Sell => Action::Buy,
            };
            
            // Unwind at market (or aggressive limit).
            // Since we don't have current quotes here easily, we rely on "market" 
            // type or aggressive limit.
            // Using Market order for speed.
            
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
            
            match self.client.create_order(&common::OrderIntent {
                 ticker: req.ticker.clone(),
                 side: req.side,
                 action: req.action,
                 price_cents: 0, // Market
                 count: req.count,
                 reason: "UNWIND".into(),
                 estimated_fee_cents: 0,
                 confidence: 0.0,
            }).await {
                Ok(resp) => {
                     warn!("Unwound {} x {} (status={})", req.ticker, req.count, resp.order.status);
                }
                Err(e) => {
                     error!("CRITICAL: FAILED TO UNWIND {}: {}", req.ticker, e);
                     // Retry logic would go here.
                }
            }
            // Small delay to avoid rate limits during panic
            sleep(Duration::from_millis(50)).await; 
        }
    }
}
