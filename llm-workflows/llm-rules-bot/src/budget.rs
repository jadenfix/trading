use std::collections::HashMap;
use std::fs::{create_dir_all, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::config::LlmBudgetConfig;

#[derive(Debug, Serialize, Deserialize)]
struct BudgetState {
    day_key: String,
    daily_calls: u32,
    per_market_calls: HashMap<String, u32>,
}

impl Default for BudgetState {
    fn default() -> Self {
        Self {
            day_key: Utc::now().format("%Y-%m-%d").to_string(),
            daily_calls: 0,
            per_market_calls: HashMap::new(),
        }
    }
}

pub struct BudgetManager {
    config: LlmBudgetConfig,
    state_path: PathBuf,
    state: BudgetState,
}

impl BudgetManager {
    pub fn load(config: LlmBudgetConfig, trades_dir: &Path) -> anyhow::Result<Self> {
        create_dir_all(trades_dir)?;
        let state_path = trades_dir.join("budget-state.json");
        let state = if state_path.exists() {
            let mut file = File::open(&state_path)?;
            let mut raw = String::new();
            file.read_to_string(&mut raw)?;
            serde_json::from_str::<BudgetState>(&raw).unwrap_or_default()
        } else {
            BudgetState::default()
        };

        Ok(Self {
            config,
            state_path,
            state,
        })
    }

    fn rollover_if_needed(&mut self) {
        let today = Utc::now().format("%Y-%m-%d").to_string();
        if self.state.day_key != today {
            self.state.day_key = today;
            self.state.daily_calls = 0;
            self.state.per_market_calls.clear();
        }
    }

    fn persist(&self) -> anyhow::Result<()> {
        let data = serde_json::to_string_pretty(&self.state)?;
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&self.state_path)?;
        file.write_all(data.as_bytes())?;
        file.flush()?;
        Ok(())
    }

    pub fn can_spend_call(&mut self, market_ticker: &str) -> bool {
        self.rollover_if_needed();
        if self.state.daily_calls >= self.config.daily_max_calls {
            return false;
        }

        let market_calls = self
            .state
            .per_market_calls
            .get(market_ticker)
            .copied()
            .unwrap_or(0);
        market_calls < self.config.per_market_max_calls
    }

    pub fn record_call(&mut self, market_ticker: &str) -> anyhow::Result<()> {
        self.rollover_if_needed();
        self.state.daily_calls = self.state.daily_calls.saturating_add(1);
        *self
            .state
            .per_market_calls
            .entry(market_ticker.to_string())
            .or_insert(0) += 1;
        self.persist()
    }

    pub fn usage_snapshot(&mut self, market_ticker: &str) -> (u32, u32, u32, u32) {
        self.rollover_if_needed();
        let market_calls = self
            .state
            .per_market_calls
            .get(market_ticker)
            .copied()
            .unwrap_or(0);
        (
            self.state.daily_calls,
            self.config.daily_max_calls,
            market_calls,
            self.config.per_market_max_calls,
        )
    }
}
