use decision_engine::TradeIntent;
use kalshi_client::KalshiRestClient;
use tracing::{error, info, warn};
use common::OrderIntent;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionOutcome {
    ShadowSkipped,
    LiveDisabled,
    Placed,
    Failed,
}

pub struct ExecutionEngine {
    client: KalshiRestClient,
    shadow_mode: bool,
    live_enable: bool,
}

impl ExecutionEngine {
    pub fn new(client: KalshiRestClient, shadow_mode: bool, live_enable: bool) -> Self {
        Self {
            client,
            shadow_mode,
            live_enable,
        }
    }

    pub async fn execute(&self, intent: TradeIntent) -> anyhow::Result<ExecutionOutcome> {
        info!("EXECUTING: {:?}", intent);

        if self.shadow_mode {
            info!("Shadow mode active - skipping live order placement for {}", intent.ticker);
            return Ok(ExecutionOutcome::ShadowSkipped);
        }

        if !self.live_enable {
            warn!(
                "Live execution disabled - skipping order placement for {}",
                intent.ticker
            );
            return Ok(ExecutionOutcome::LiveDisabled);
        }

        // Convert TradeIntent (from decision-engine) to OrderIntent (from common)
        let order_intent = OrderIntent {
            ticker: intent.ticker,
            side: intent.side,
            action: intent.action,
            price_cents: intent.price_cents,
            count: intent.size_contracts,
            reason: intent.reasons.join("; "),
            estimated_fee_cents: 0, // TODO: Calculate fee if needed for logs
            confidence: intent.confidence,
        };

        // Place order
        // Note: create_order might fail. We should log error but not crash.
        match self.client.create_order(&order_intent).await {
            Ok(resp) => {
                info!("Order placed successfully: ID={} Status={}", 
                      resp.order.order_id, resp.order.status);
                Ok(ExecutionOutcome::Placed)
            }
            Err(e) => {
                error!("Order placement failed for {}: {:?}", order_intent.ticker, e);
                Ok(ExecutionOutcome::Failed)
            }
        }
    }
}
