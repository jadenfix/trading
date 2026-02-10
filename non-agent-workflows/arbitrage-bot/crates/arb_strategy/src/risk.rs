//! Risk manager — enforces exposure limits and anti-spam gates.

use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use common::Error;
use tracing::{info, warn};

use crate::arb::ArbOpportunity;
use crate::config::ArbRiskConfig;

pub struct ArbRiskManager {
    config: ArbRiskConfig,
    /// Track exposure per event ticker (cents).
    event_exposure: HashMap<String, i64>,
    /// Track total portfolio exposure (cents).
    total_exposure: i64,
    /// Attempt timestamps per group (for rate limiting).
    attempt_history: HashMap<String, VecDeque<Instant>>,
}

impl ArbRiskManager {
    pub fn new(config: ArbRiskConfig) -> Self {
        Self {
            config,
            event_exposure: HashMap::new(),
            total_exposure: 0,
            attempt_history: HashMap::new(),
        }
    }

    /// Check if an arb opportunity is safe to execute.
    pub fn check(&mut self, opp: &ArbOpportunity) -> Result<(), Error> {
        let now = Instant::now();

        // 1. Check attempt frequency (spam protection).
        let history = self
            .attempt_history
            .entry(opp.group_event_ticker.clone())
            .or_default();
        
        // Prune old attempts (> 60s).
        while history.front().map_or(false, |t| t.elapsed().as_secs() > 60) {
            history.pop_front();
        }

        if history.len() >= self.config.max_attempts_per_group_per_min as usize {
            warn!("{}: max attempts limit reached", opp.group_event_ticker);
            return Err(Error::RiskViolation("Too many attempts per minute".into()));
        }

        // 2. Check Event Exposure.
        let current_event_exposure = *self
            .event_exposure
            .get(&opp.group_event_ticker)
            .unwrap_or(&0);
        
        // Conservative exposure add: for arb, we are hedged, but gross exposure matters
        // for "what if one leg fails".
        // Gross exposure of the trade = qty * payout (100) per leg? 
        // Or just the max liability.
        // For Buy-Set, max liability is Cost.
        // For Sell-Set, max liability is Payout * derived_qty - Premium.
        // Let's use `payout * qty` as a conservative proxy for specific-event risk.
        let trade_risk = 100 * opp.qty; 

        if current_event_exposure + trade_risk > self.config.max_exposure_per_event_cents {
             return Err(Error::RiskViolation(format!(
                 "Event exposure limit: {} + {} > {}", 
                 current_event_exposure, trade_risk, self.config.max_exposure_per_event_cents
             )));
        }

        // 3. Check Total Exposure.
        if self.total_exposure + trade_risk > self.config.max_total_exposure_cents {
             return Err(Error::RiskViolation(format!(
                 "Total exposure limit: {} + {} > {}", 
                 self.total_exposure, trade_risk, self.config.max_total_exposure_cents
             )));
        }

        // Approved. Record the attempt.
        history.push_back(now);
        
        Ok(())
    }

    /// Record a successful execution to update exposure tracking.
    pub fn record_execution(&mut self, opp: &ArbOpportunity) {
        let trade_risk = 100 * opp.qty;
        *self.event_exposure.entry(opp.group_event_ticker.clone()).or_default() += trade_risk;
        self.total_exposure += trade_risk;
        
        info!(
            "Risk updated: event_exposure={} total={}",
            self.event_exposure[&opp.group_event_ticker], self.total_exposure
        );
    }
    
    /// Reset exposure tracking (e.g. after position reconciliation).
    pub fn reset_exposure(&mut self) {
        self.event_exposure.clear();
        self.total_exposure = 0;
    }
}
