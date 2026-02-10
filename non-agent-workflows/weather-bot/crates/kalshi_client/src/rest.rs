//! REST client for the Kalshi API.
//!
//! Covers: market discovery, order management, portfolio queries.
//! All methods are rate-limited and authenticated via RSA-PSS.

use common::{
    BalanceResponse, CreateOrderRequest, CreateOrderResponse, Error, MarketInfo, MarketsResponse,
    OrderIntent, PositionsResponse, Side, Action, OrderType,
};
use tracing::{debug, warn};
use uuid::Uuid;

use crate::auth::KalshiAuth;
use crate::rate_limit::RateLimiter;

/// Async REST client for Kalshi trade API.
#[derive(Debug, Clone)]
pub struct KalshiRestClient {
    client: reqwest::Client,
    auth: KalshiAuth,
    base_url: String,
    limiter: RateLimiter,
}

impl KalshiRestClient {
    /// Create a new REST client.
    ///
    /// * `use_demo` — if true, points to `https://demo-api.kalshi.co`.
    pub fn new(auth: KalshiAuth, use_demo: bool) -> Self {
        let base_url = if use_demo {
            "https://demo-api.kalshi.co".to_string()
        } else {
            "https://api.elections.kalshi.com".to_string()
        };

        let client = reqwest::Client::builder()
            .pool_max_idle_per_host(4)
            .tcp_keepalive(std::time::Duration::from_secs(30))
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("failed to build reqwest client");

        Self {
            client,
            auth,
            base_url,
            limiter: RateLimiter::new(),
        }
    }

    /// URL helper.
    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    // ── Read endpoints ────────────────────────────────────────────────

    /// Fetch open markets matching a series ticker prefix.
    ///
    /// Handles pagination automatically and returns all matching markets.
    pub async fn get_markets(
        &self,
        series_ticker: Option<&str>,
        status: Option<&str>,
        limit: u32,
    ) -> Result<Vec<MarketInfo>, Error> {
        let mut all_markets = Vec::new();
        let mut cursor: Option<String> = None;

        loop {
            self.limiter.wait_read().await;

            let path = "/trade-api/v2/markets";
            let headers = self.auth.headers("GET", path);

            let mut req = self.client.get(self.url(path)).headers(headers);

            if let Some(st) = series_ticker {
                req = req.query(&[("series_ticker", st)]);
            }
            if let Some(s) = status {
                req = req.query(&[("status", s)]);
            }
            req = req.query(&[("limit", &limit.to_string())]);
            if let Some(ref c) = cursor {
                req = req.query(&[("cursor", c.as_str())]);
            }

            let resp = req
                .send()
                .await
                .map_err(|e| Error::Http(e.to_string()))?;

            let status_code = resp.status().as_u16();
            if status_code != 200 {
                let body = resp.text().await.unwrap_or_default();
                return Err(Error::KalshiApi {
                    status: status_code,
                    message: body,
                });
            }

            let body: MarketsResponse = resp
                .json()
                .await
                .map_err(|e| Error::Http(e.to_string()))?;

            let count = body.markets.len();
            all_markets.extend(body.markets);

            debug!("Fetched {} markets (total: {})", count, all_markets.len());

            match body.cursor {
                Some(c) if !c.is_empty() => cursor = Some(c),
                _ => break,
            }
        }

        Ok(all_markets)
    }

    /// Fetch a single market by ticker.
    pub async fn get_market(&self, ticker: &str) -> Result<MarketInfo, Error> {
        self.limiter.wait_read().await;

        let path = format!("/trade-api/v2/markets/{}", ticker);
        let headers = self.auth.headers("GET", &path);

        let resp = self
            .client
            .get(self.url(&path))
            .headers(headers)
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;

        let status_code = resp.status().as_u16();
        if status_code != 200 {
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::KalshiApi {
                status: status_code,
                message: body,
            });
        }

        #[derive(serde::Deserialize)]
        struct Wrapper {
            market: MarketInfo,
        }

        let w: Wrapper = resp
            .json()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;

        Ok(w.market)
    }

    /// Get portfolio balance in cents.
    pub async fn get_balance(&self) -> Result<i64, Error> {
        self.limiter.wait_read().await;

        let path = "/trade-api/v2/portfolio/balance";
        let headers = self.auth.headers("GET", path);

        let resp = self
            .client
            .get(self.url(path))
            .headers(headers)
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;

        let status_code = resp.status().as_u16();
        if status_code != 200 {
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::KalshiApi {
                status: status_code,
                message: body,
            });
        }

        let bal: BalanceResponse = resp
            .json()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;

        Ok(bal.balance)
    }

    /// Get all portfolio positions.
    pub async fn get_positions(&self) -> Result<Vec<common::Position>, Error> {
        let mut all_positions = Vec::new();
        let mut cursor: Option<String> = None;

        loop {
            self.limiter.wait_read().await;

            let path = "/trade-api/v2/portfolio/positions";
            let headers = self.auth.headers("GET", path);

            let mut req = self
                .client
                .get(self.url(path))
                .headers(headers)
                .query(&[("limit", "200")]);

            if let Some(ref c) = cursor {
                req = req.query(&[("cursor", c.as_str())]);
            }

            let resp = req
                .send()
                .await
                .map_err(|e| Error::Http(e.to_string()))?;

            let status_code = resp.status().as_u16();
            if status_code != 200 {
                let body = resp.text().await.unwrap_or_default();
                return Err(Error::KalshiApi {
                    status: status_code,
                    message: body,
                });
            }

            let body: PositionsResponse = resp
                .json()
                .await
                .map_err(|e| Error::Http(e.to_string()))?;

            let count = body.market_positions.len();
            all_positions.extend(body.market_positions);

            debug!("Fetched {} positions (total: {})", count, all_positions.len());

            match body.cursor {
                Some(c) if !c.is_empty() => cursor = Some(c),
                _ => break,
            }
        }

        Ok(all_positions)
    }

    // ── Write endpoints ───────────────────────────────────────────────

    /// Place an order via the Kalshi API.
    pub async fn create_order(
        &self,
        intent: &OrderIntent,
    ) -> Result<CreateOrderResponse, Error> {
        self.limiter.wait_write().await;

        let path = "/trade-api/v2/portfolio/orders";
        let headers = self.auth.headers("POST", path);

        let client_order_id = Uuid::new_v4().to_string();

        let (yes_price, no_price) = match intent.side {
            Side::Yes => (Some(intent.price_cents), None),
            Side::No => (None, Some(intent.price_cents)),
        };

        let body = CreateOrderRequest {
            ticker: intent.ticker.clone(),
            side: intent.side,
            action: intent.action,
            client_order_id: client_order_id.clone(),
            count: intent.count,
            order_type: OrderType::Limit,
            yes_price,
            no_price,
            expiration_ts: None,
        };

        debug!(
            "Creating order: {} {} {} @ {}¢ x{} ({})",
            match intent.action { Action::Buy => "BUY", Action::Sell => "SELL" },
            match intent.side { Side::Yes => "YES", Side::No => "NO" },
            intent.ticker,
            intent.price_cents,
            intent.count,
            intent.reason,
        );

        let resp = self
            .client
            .post(self.url(path))
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;

        let status_code = resp.status().as_u16();
        if status_code == 429 {
            warn!("Rate limited on order creation");
            return Err(Error::RateLimited { retry_after_ms: 1000 });
        }
        if status_code != 200 && status_code != 201 {
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::KalshiApi {
                status: status_code,
                message: body,
            });
        }

        let order_resp: CreateOrderResponse = resp
            .json()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;

        debug!(
            "Order placed: id={} status={} fill={}",
            order_resp.order.order_id,
            order_resp.order.status,
            order_resp.order.fill_count,
        );

        Ok(order_resp)
    }

    /// Cancel an order by its order ID.
    pub async fn cancel_order(&self, order_id: &str) -> Result<(), Error> {
        self.limiter.wait_write().await;

        let path = format!("/trade-api/v2/portfolio/orders/{}", order_id);
        let headers = self.auth.headers("DELETE", &path);

        let resp = self
            .client
            .delete(self.url(&path))
            .headers(headers)
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;

        let status_code = resp.status().as_u16();
        if status_code != 200 && status_code != 204 {
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::KalshiApi {
                status: status_code,
                message: body,
            });
        }

        debug!("Cancelled order: {}", order_id);
        Ok(())
    }
}
