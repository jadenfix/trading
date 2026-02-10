//! Rate limiter for Kalshi API.
//!
//! Basic tier limits: 20 reads/sec, 10 writes/sec.

use governor::{Quota, RateLimiter as GovLimiter};
use std::num::NonZeroU32;
use std::sync::Arc;

/// Dual rate limiter — separate buckets for reads and writes.
#[derive(Debug, Clone)]
pub struct RateLimiter {
    read_limiter: Arc<GovLimiter<governor::state::NotKeyed, governor::state::InMemoryState, governor::clock::DefaultClock>>,
    write_limiter: Arc<GovLimiter<governor::state::NotKeyed, governor::state::InMemoryState, governor::clock::DefaultClock>>,
}

impl RateLimiter {
    /// Create with Kalshi basic-tier limits.
    pub fn new() -> Self {
        Self::with_limits(20, 10)
    }

    /// Create with custom per-second limits.
    pub fn with_limits(reads_per_sec: u32, writes_per_sec: u32) -> Self {
        let read_quota = Quota::per_second(NonZeroU32::new(reads_per_sec).unwrap());
        let write_quota = Quota::per_second(NonZeroU32::new(writes_per_sec).unwrap());

        Self {
            read_limiter: Arc::new(GovLimiter::direct(read_quota)),
            write_limiter: Arc::new(GovLimiter::direct(write_quota)),
        }
    }

    /// Wait until a read slot is available.
    pub async fn wait_read(&self) {
        self.read_limiter.until_ready().await;
    }

    /// Wait until a write slot is available.
    pub async fn wait_write(&self) {
        self.write_limiter.until_ready().await;
    }

    /// Try to acquire a read slot without waiting. Returns true if acquired.
    pub fn try_read(&self) -> bool {
        self.read_limiter.check().is_ok()
    }

    /// Try to acquire a write slot without waiting. Returns true if acquired.
    pub fn try_write(&self) -> bool {
        self.write_limiter.check().is_ok()
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}
