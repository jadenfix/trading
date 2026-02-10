//! Universe builder — discovers events and groups markets into arb sets.
//!
//! Fetches events with nested markets from the Kalshi API, classifies
//! them as binary-outcome, and builds `ArbGroup` structs representing
//! candidate complete sets for arbitrage.

use std::collections::HashMap;

use common::{EventInfo, MarketInfo};
use tracing::{debug, info, warn};

use kalshi_client::KalshiRestClient;

// ── Public Types ──────────────────────────────────────────────────────

/// A candidate arb group — a set of markets under one event where
/// exactly one outcome should resolve YES.
#[derive(Debug, Clone)]
pub struct ArbGroup {
    /// Event ticker (parent).
    pub event_ticker: String,
    /// Human-readable event title.
    pub title: String,
    /// Market tickers in this group (the outcomes).
    pub tickers: Vec<String>,
    /// Market info for each ticker.
    pub markets: HashMap<String, MarketInfo>,
    /// Whether the API says outcomes are mutually exclusive.
    pub is_mutually_exclusive: bool,
    /// Whether we believe the set is exhaustive (all outcomes covered).
    pub is_exhaustive: bool,
    /// Edge case flag if the set might not be clean.
    pub edge_case: Option<EdgeCase>,
}

/// Possible edge cases in a market group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EdgeCase {
    /// A tie/draw outcome exists but no tie market is in the set.
    TiePossible,
    /// The event could be voided.
    VoidPossible,
    /// Not all outcomes are available (partial market listing).
    PartialSet,
}

// ── Event API types ───────────────────────────────────────────────────

// ── Event API types ──────────────────────────────────────────────────

// Used from common::EventInfo and common::EventsResponse

// ── Universe Builder ──────────────────────────────────────────────────

/// Discovers events and builds candidate arb groups.
pub struct UniverseBuilder;

impl UniverseBuilder {
    /// Fetch all open events with nested markets and build arb groups.
    pub async fn build(client: &KalshiRestClient) -> Vec<ArbGroup> {
        info!("Building universe: fetching events with nested markets...");

        let events = match Self::fetch_events(client).await {
            Ok(e) => e,
            Err(e) => {
                warn!("Failed to fetch events: {}", e);
                return Vec::new();
            }
        };

        info!("Fetched {} events", events.len());

        let mut groups = Vec::new();

        for event in events {
            // Only consider open markets.
            let open_markets: Vec<MarketInfo> = event
                .markets
                .iter()
                .filter(|m| m.status == "active" || m.status == "open")
                .cloned()
                .collect();

            if open_markets.is_empty() {
                continue;
            }

            // A single binary market is a valid arb group (YES vs NO).
            // Multiple markets in an event (categorical) are also valid.

            // Classify exhaustiveness.
            let (is_exhaustive, edge_case) =
                Self::classify_exhaustiveness(&event.mutually_exclusive, &open_markets);

            let tickers: Vec<String> = open_markets.iter().map(|m| m.ticker.clone()).collect();
            let markets: HashMap<String, MarketInfo> = open_markets
                .into_iter()
                .map(|m| (m.ticker.clone(), m))
                .collect();

            // debug!(
            //     "ArbGroup: {} — {} outcomes, ME={}, exhaustive={}, edge={:?}",
            //     event.event_ticker,
            //     tickers.len(),
            //     event.mutually_exclusive,
            //     is_exhaustive,
            //     edge_case
            // );

            groups.push(ArbGroup {
                event_ticker: event.event_ticker,
                title: event.title,
                tickers,
                markets,
                is_mutually_exclusive: event.mutually_exclusive,
                is_exhaustive,
                edge_case,
            });
        }

        info!(
            "Universe: {} candidate arb groups from {} events",
            groups.len(),
            groups.len()
        );

        groups
    }

    /// Fetch events from Kalshi API with pagination.
    async fn fetch_events(client: &KalshiRestClient) -> Result<Vec<EventInfo>, common::Error> {
        let mut all_events = Vec::new();
        let mut cursor: Option<String> = None;

        loop {
            let (events, next_cursor) = client
                .get_events(Some("open"), true, Some(200), cursor.as_deref())
                .await?;

            let count = events.len();
            all_events.extend(events);

            debug!("Fetched {} events (total: {})", count, all_events.len());

            match next_cursor {
                Some(c) if !c.is_empty() => cursor = Some(c),
                _ => break,
            }
        }

        Ok(all_events)
    }

    /// Determine if a set of markets is exhaustive.
    fn classify_exhaustiveness(
        mutually_exclusive: &bool,
        markets: &[MarketInfo],
    ) -> (bool, Option<EdgeCase>) {
        // Special case: Single market is self-contained (YES/NO arb).
        // It is always "mutually exclusive" in the sense that you can't have both terminate YES (unless void).
        // It is "exhaustive" because YES and NO cover all non-void outcomes.
        if markets.len() == 1 {
            return (true, None);
        }

        if !mutually_exclusive {
            return (false, Some(EdgeCase::PartialSet));
        }

        // Heuristic: check rules for tie/draw/void keywords.
        let mut has_tie_market = false;
        let mut tie_mentioned = false;

        for market in markets {
            let title_lower = market.title.to_lowercase();
            if title_lower.contains("tie") || title_lower.contains("draw") {
                has_tie_market = true;
            }

            let rules = format!(
                "{} {} {}",
                title_lower,
                market.rules_primary.to_lowercase(),
                market.rules_secondary.to_lowercase(),
            );

            // Void check still uses everything
            if rules.contains("void") || rules.contains("cancel") {
                return (false, Some(EdgeCase::VoidPossible));
            }
        }

        // Check broader event rules for tie possibility.
        for market in markets {
            let rules = format!(
                "{} {}",
                market.rules_primary.to_lowercase(),
                market.rules_secondary.to_lowercase(),
            );
            if (rules.contains("tie") || rules.contains("draw")) && !has_tie_market {
                tie_mentioned = true;
            }
        }

        if tie_mentioned && !has_tie_market {
            return (false, Some(EdgeCase::TiePossible));
        }

        // If mutually exclusive and no edge cases detected, assume exhaustive.
        (true, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_market(ticker: &str, title: &str) -> MarketInfo {
        MarketInfo {
            ticker: ticker.into(),
            event_ticker: "EVT-TEST".into(),
            market_type: "binary".into(),
            title: title.into(),
            subtitle: String::new(),
            status: "open".into(),
            yes_bid: 40,
            yes_ask: 45,
            no_bid: 55,
            no_ask: 60,
            last_price: 42,
            volume: 100,
            volume_24h: 50,
            open_interest: 10,
            rules_primary: String::new(),
            rules_secondary: String::new(),
            close_time: None,
            expiration_time: None,
            result: None,
            strike_type: None,
            floor_strike: None,
            cap_strike: None,
            functional_strike: None,
            tick_size: Some(1),
            response_price_units: None,
        }
    }

    #[test]
    fn test_exhaustive_mutually_exclusive() {
        let markets = vec![
            make_market("A-YES", "Team A wins"),
            make_market("B-YES", "Team B wins"),
        ];
        let (exhaustive, edge) = UniverseBuilder::classify_exhaustiveness(&true, &markets);
        assert!(exhaustive);
        assert!(edge.is_none());
    }

    #[test]
    fn test_non_mutually_exclusive() {
        let markets = vec![
            make_market("A-YES", "Team A wins"),
            make_market("B-YES", "Team B wins"),
        ];
        let (exhaustive, edge) = UniverseBuilder::classify_exhaustiveness(&false, &markets);
        assert!(!exhaustive);
        assert_eq!(edge, Some(EdgeCase::PartialSet));
    }

    #[test]
    fn test_void_detected() {
        let mut m = make_market("A-YES", "Team A wins");
        m.rules_primary = "If the event is void or cancelled, all contracts resolve NO.".into();
        let markets = vec![m, make_market("B-YES", "Team B wins")];
        let (exhaustive, edge) = UniverseBuilder::classify_exhaustiveness(&true, &markets);
        assert!(!exhaustive);
        assert_eq!(edge, Some(EdgeCase::VoidPossible));
    }

    #[test]
    fn test_tie_without_tie_market() {
        let mut m = make_market("A-YES", "Team A wins");
        m.rules_primary = "In the event of a tie, all contracts resolve NO.".into();
        let markets = vec![m, make_market("B-YES", "Team B wins")];
        let (exhaustive, edge) = UniverseBuilder::classify_exhaustiveness(&true, &markets);
        assert!(!exhaustive);
        assert_eq!(edge, Some(EdgeCase::TiePossible));
    }

    #[test]
    fn test_tie_with_tie_market() {
        let mut m = make_market("A-YES", "Team A wins");
        m.rules_primary = "In the event of a tie, the tie market resolves YES.".into();
        let markets = vec![
            m,
            make_market("B-YES", "Team B wins"),
            make_market("TIE-YES", "Tie / Draw"),
        ];
        let (exhaustive, edge) = UniverseBuilder::classify_exhaustiveness(&true, &markets);
        // Has tie market, so the "tie" keyword is accounted for.
        assert!(exhaustive);
        assert!(edge.is_none());
    }
}
