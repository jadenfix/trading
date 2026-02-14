use std::collections::VecDeque;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use common::Position;

use crate::config::RiskConfig;

pub struct RiskGuard {
    config: RiskConfig,
    session_start_balance: Option<i64>,
    order_timestamps: VecDeque<Instant>,
}

impl RiskGuard {
    pub fn new(config: RiskConfig) -> Self {
        Self {
            config,
            session_start_balance: None,
            order_timestamps: VecDeque::new(),
        }
    }

    fn prune_orders(&mut self) {
        let cutoff = Instant::now() - Duration::from_secs(60);
        while self.order_timestamps.front().is_some_and(|ts| *ts < cutoff) {
            self.order_timestamps.pop_front();
        }
    }

    pub fn record_order(&mut self) {
        self.prune_orders();
        self.order_timestamps.push_back(Instant::now());
    }

    pub fn check_buy(
        &mut self,
        ticker: &str,
        price_cents: i64,
        count: i64,
        positions: &[Position],
        balance_cents: i64,
    ) -> Result<()> {
        if self.session_start_balance.is_none() {
            self.session_start_balance = Some(balance_cents);
        }

        if count <= 0 {
            bail!("count must be positive");
        }
        if !(1..=99).contains(&price_cents) {
            bail!("price must be in [1,99]");
        }

        self.prune_orders();
        if self.order_timestamps.len() as u32 >= self.config.max_orders_per_minute {
            bail!(
                "order throttle exceeded: {} >= {}",
                self.order_timestamps.len(),
                self.config.max_orders_per_minute
            );
        }

        let order_cost = price_cents.saturating_mul(count);
        if order_cost > self.config.max_position_cents {
            bail!(
                "order exceeds max_position_cents: {} > {}",
                order_cost,
                self.config.max_position_cents
            );
        }

        let current_market_exposure = positions
            .iter()
            .find(|p| p.ticker == ticker)
            .map(|p| p.market_exposure.abs())
            .unwrap_or(0);
        if current_market_exposure.saturating_add(order_cost) > self.config.max_position_cents {
            bail!(
                "position limit exceeded for {}: {} + {} > {}",
                ticker,
                current_market_exposure,
                order_cost,
                self.config.max_position_cents
            );
        }

        let total_exposure: i64 = positions.iter().map(|p| p.market_exposure.abs()).sum();
        if total_exposure.saturating_add(order_cost) > self.config.max_total_exposure_cents {
            bail!(
                "total exposure exceeded: {} + {} > {}",
                total_exposure,
                order_cost,
                self.config.max_total_exposure_cents
            );
        }

        if balance_cents.saturating_sub(order_cost) < self.config.min_balance_cents {
            bail!(
                "min balance breached: {} - {} < {}",
                balance_cents,
                order_cost,
                self.config.min_balance_cents
            );
        }

        if let Some(start) = self.session_start_balance {
            let drawdown = (start - balance_cents).max(0);
            if drawdown > self.config.max_daily_loss_cents {
                bail!(
                    "drawdown exceeded: {} > {}",
                    drawdown,
                    self.config.max_daily_loss_cents
                );
            }
        }

        Ok(())
    }
}
