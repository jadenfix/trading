//! Quote book — wraps the shared PriceCache with arb-specific helpers.
//!
//! Provides group-level quote snapshots, freshness checks, and
//! liquidity gating for the arb evaluator.

use std::time::Duration;

use kalshi_client::{PriceCache, PriceEntry};
use tracing::debug;

/// Arb-specific view over the shared price cache.
#[derive(Clone)]
pub struct QuoteBook {
    cache: PriceCache,
}

/// A snapshot of quotes for one arb leg.
#[derive(Debug, Clone)]
pub struct MarketQuote {
    pub ticker: String,
    pub yes_bid: i64,
    pub yes_ask: i64,
    pub last_price: i64,
    pub volume: i64,
    pub age_ms: u64,
}

impl QuoteBook {
    pub fn new(cache: PriceCache) -> Self {
        Self { cache }
    }

    /// Get the underlying PriceCache (for WS client).
    pub fn inner(&self) -> &PriceCache {
        &self.cache
    }

    /// Snapshot quotes for all tickers in a group.
    ///
    /// Returns `None` if any ticker is missing from the cache.
    pub async fn group_quotes(&self, tickers: &[String]) -> Option<Vec<MarketQuote>> {
        let cache = self.cache.read().await;
        let mut quotes = Vec::with_capacity(tickers.len());

        for ticker in tickers {
            match cache.get(ticker) {
                Some(entry) => {
                    quotes.push(MarketQuote {
                        ticker: ticker.clone(),
                        yes_bid: entry.yes_bid,
                        yes_ask: entry.yes_ask,
                        last_price: entry.last_price,
                        volume: entry.volume,
                        age_ms: entry.updated_at.elapsed().as_millis() as u64,
                    });
                }
                None => {
                    debug!("{}: no quote data", ticker);
                    return None;
                }
            }
        }

        Some(quotes)
    }

    /// Check if all quotes in a group are fresh (within max_age_secs).
    pub fn all_fresh(quotes: &[MarketQuote], max_age_secs: u64) -> bool {
        let max_age_ms = max_age_secs * 1000;
        quotes.iter().all(|q| q.age_ms <= max_age_ms)
    }

    /// Check if a specific ticker's quote is fresh.
    pub async fn is_fresh(&self, ticker: &str, max_age_secs: u64) -> bool {
        let cache = self.cache.read().await;
        match cache.get(ticker) {
            Some(entry) => entry.updated_at.elapsed() <= Duration::from_secs(max_age_secs),
            None => false,
        }
    }

    /// Get a single quote for a ticker.
    pub async fn get(&self, ticker: &str) -> Option<PriceEntry> {
        let cache = self.cache.read().await;
        cache.get(ticker).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Instant;
    use tokio::sync::RwLock;

    fn make_cache_with(entries: Vec<(&str, i64, i64)>) -> PriceCache {
        let mut map = HashMap::new();
        for (ticker, bid, ask) in entries {
            map.insert(
                ticker.to_string(),
                PriceEntry {
                    yes_bid: bid,
                    yes_ask: ask,
                    last_price: (bid + ask) / 2,
                    volume: 100,
                    updated_at: Instant::now(),
                },
            );
        }
        Arc::new(RwLock::new(map))
    }

    #[tokio::test]
    async fn test_group_quotes_all_present() {
        let cache = make_cache_with(vec![("A", 40, 45), ("B", 50, 55)]);
        let qb = QuoteBook::new(cache);
        let tickers = vec!["A".into(), "B".into()];
        let quotes = qb.group_quotes(&tickers).await;
        assert!(quotes.is_some());
        assert_eq!(quotes.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_group_quotes_missing_ticker() {
        let cache = make_cache_with(vec![("A", 40, 45)]);
        let qb = QuoteBook::new(cache);
        let tickers = vec!["A".into(), "B".into()];
        let quotes = qb.group_quotes(&tickers).await;
        assert!(quotes.is_none());
    }

    #[test]
    fn test_all_fresh() {
        let quotes = vec![
            MarketQuote {
                ticker: "A".into(),
                yes_bid: 40,
                yes_ask: 45,
                last_price: 42,
                volume: 100,
                age_ms: 500,
            },
            MarketQuote {
                ticker: "B".into(),
                yes_bid: 50,
                yes_ask: 55,
                last_price: 52,
                volume: 100,
                age_ms: 1500,
            },
        ];
        assert!(QuoteBook::all_fresh(&quotes, 2));
        assert!(!QuoteBook::all_fresh(&quotes, 1));
    }
}
