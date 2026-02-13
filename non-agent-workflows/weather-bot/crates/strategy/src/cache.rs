//! In-memory caches for forecast data.
//!
//! Uses `DashMap` for lock-free concurrent reads — the hot path
//! (strategy evaluation) only reads, so this avoids contention.

use common::ForecastData;
use dashmap::DashMap;
use std::sync::Arc;
use std::time::Instant;

/// A cached forecast entry with staleness tracking.
#[derive(Debug, Clone)]
pub struct ForecastEntry {
    /// Blended ensemble forecast used for pricing.
    pub ensemble: ForecastData,
    /// Raw NOAA source forecast (if available).
    pub noaa: Option<ForecastData>,
    /// Raw Google source forecast (if available).
    pub google: Option<ForecastData>,
    pub updated_at: Instant,
}

impl ForecastEntry {
    pub fn is_stale(&self, max_age_secs: u64) -> bool {
        self.updated_at.elapsed().as_secs() > max_age_secs
    }
}

/// Thread-safe forecast cache keyed by city name.
pub type ForecastCache = Arc<DashMap<String, ForecastEntry>>;

/// Create a new empty ForecastCache.
pub fn new_forecast_cache() -> ForecastCache {
    Arc::new(DashMap::new())
}
