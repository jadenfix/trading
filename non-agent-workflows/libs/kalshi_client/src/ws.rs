//! WebSocket client for the Kalshi ticker feed.
//!
//! Connects to the public `ticker` channel via
//! `wss://api.elections.kalshi.com/trade-api/ws/v2` (or demo equivalent),
//! and streams price updates into a shared `PriceCache`.
//! Endpoint can be overridden with `KALSHI_WS_URL`.

use std::collections::HashMap;
use std::error::Error as StdError;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{WsMessage, WsSubscribeCmd, WsSubscribeParams, WsTickerMessage};
use futures_util::{Sink, SinkExt, StreamExt};
use tokio::sync::RwLock;
use tokio::time::sleep;
use tokio_tungstenite::tungstenite;
use tracing::{debug, error, info, warn};

use crate::auth::KalshiAuth;

const DEMO_WS_URL: &str = "wss://demo-api.kalshi.co/trade-api/ws/v2";
const PROD_WS_URL: &str = "wss://api.elections.kalshi.com/trade-api/ws/v2";

fn normalize_ws_url(raw: &str) -> String {
    raw.trim().trim_end_matches('/').to_string()
}

fn resolve_ws_url(use_demo: bool) -> String {
    if let Ok(override_url) = std::env::var("KALSHI_WS_URL") {
        let normalized = normalize_ws_url(&override_url);
        if !normalized.is_empty() {
            info!("Using KALSHI_WS_URL override: {}", normalized);
            return normalized;
        }
        warn!("Ignoring empty KALSHI_WS_URL override");
    }

    if use_demo {
        DEMO_WS_URL.to_string()
    } else {
        PROD_WS_URL.to_string()
    }
}

fn format_error_chain(err: &dyn StdError) -> String {
    let mut message = err.to_string();
    let mut source = err.source();

    while let Some(cause) = source {
        let cause_msg = cause.to_string();
        if !cause_msg.is_empty() && !message.contains(&cause_msg) {
            message.push_str(": ");
            message.push_str(&cause_msg);
        }
        source = cause.source();
    }

    message
}

fn env_truthy(name: &str) -> bool {
    match std::env::var(name) {
        Ok(v) => {
            let v = v.trim().to_ascii_lowercase();
            !v.is_empty() && v != "0" && v != "false" && v != "no"
        }
        Err(_) => false,
    }
}

/// A cached price entry updated by the WebSocket feed.
#[derive(Debug, Clone)]
pub struct PriceEntry {
    pub yes_bid: i64,
    pub yes_ask: i64,
    pub last_price: i64,
    pub volume: i64,
    pub updated_at: Instant,
}

/// Thread-safe price cache — ticker → PriceEntry.
pub type PriceCache = Arc<RwLock<HashMap<String, PriceEntry>>>;

/// Create a new empty PriceCache.
pub fn new_price_cache() -> PriceCache {
    Arc::new(RwLock::new(HashMap::new()))
}

/// Kalshi WebSocket client that maintains a persistent connection
/// and updates the PriceCache on each ticker message.
pub struct KalshiWsClient {
    auth: KalshiAuth,
    ws_url: String,
    price_cache: PriceCache,
}

impl KalshiWsClient {
    async fn sync_subscriptions<S>(
        write: &mut S,
        tickers: &Arc<RwLock<Vec<String>>>,
        subscribed_tickers: &mut Vec<String>,
        sub_id: &mut u64,
    ) -> Result<(), common::Error>
    where
        S: Sink<tungstenite::Message> + Unpin,
        S::Error: StdError + Send + Sync + 'static,
    {
        let latest_tickers = tickers.read().await.clone();

        if latest_tickers.is_empty() {
            if subscribed_tickers.is_empty() {
                debug!("WS subscription pending: no tickers discovered yet");
            }
            return Ok(());
        }

        if *subscribed_tickers == latest_tickers {
            return Ok(());
        }

        let sub = WsSubscribeCmd {
            id: *sub_id,
            cmd: "subscribe".to_string(),
            params: WsSubscribeParams {
                channels: vec!["ticker".to_string()],
                market_tickers: Some(latest_tickers.clone()),
            },
        };

        let sub_json = serde_json::to_string(&sub)
            .map_err(|e| common::Error::WebSocket(format_error_chain(&e)))?;

        write
            .send(tungstenite::Message::Text(sub_json))
            .await
            .map_err(|e| common::Error::WebSocket(format_error_chain(&e)))?;

        info!(
            "Subscribed to {} tickers (subscription id={})",
            latest_tickers.len(),
            *sub_id
        );

        *subscribed_tickers = latest_tickers;
        *sub_id += 1;

        Ok(())
    }

    pub fn new(auth: KalshiAuth, use_demo: bool, price_cache: PriceCache) -> Self {
        let ws_url = resolve_ws_url(use_demo);

        Self {
            auth,
            ws_url,
            price_cache,
        }
    }

    /// Run the WebSocket event loop forever, auto-reconnecting on failure.
    ///
    /// `tickers` is a shared list of tickers to subscribe to; the caller
    /// can update it when market discovery runs.
    pub async fn run(&self, tickers: Arc<RwLock<Vec<String>>>) {
        let mut backoff = Duration::from_secs(1);

        loop {
            info!("Connecting to Kalshi WebSocket: {}", self.ws_url);

            match self.connect_and_stream(&tickers).await {
                Ok(()) => {
                    info!("WebSocket connection closed cleanly");
                    backoff = Duration::from_secs(1);
                }
                Err(e) => {
                    error!("WebSocket error: {}. Reconnecting in {:?}", e, backoff);
                }
            }

            sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(60));
        }
    }

    async fn connect_and_stream(
        &self,
        tickers: &Arc<RwLock<Vec<String>>>,
    ) -> Result<(), common::Error> {
        let url = url::Url::parse(&self.ws_url)
            .map_err(|e| common::Error::WebSocket(format_error_chain(&e)))?;
        let host = url.host_str().ok_or_else(|| {
            common::Error::WebSocket(format!("WebSocket URL missing host: {}", self.ws_url))
        })?;
        let host_header = if let Some(port) = url.port() {
            format!("{}:{}", host, port)
        } else {
            host.to_string()
        };

        let path_to_sign = {
            let p = url.path();
            if p.is_empty() {
                "/"
            } else {
                p
            }
        };

        let ws_stream = if env_truthy("KALSHI_WS_DISABLE_AUTH") {
            info!("Connecting WebSocket without auth (KALSHI_WS_DISABLE_AUTH enabled)");
            let (stream, _) = tokio_tungstenite::connect_async(self.ws_url.as_str())
                .await
                .map_err(|e| common::Error::WebSocket(format_error_chain(&e)))?;
            stream
        } else {
            let (timestamp, signature) = self.auth.sign_request("GET", path_to_sign);

            let request = tungstenite::http::Request::builder()
                .uri(self.ws_url.as_str())
                .header("KALSHI-ACCESS-KEY", &self.auth.api_key)
                .header("KALSHI-ACCESS-TIMESTAMP", &timestamp)
                .header("KALSHI-ACCESS-SIGNATURE", &signature)
                .header("Host", host_header)
                .header("Connection", "Upgrade")
                .header("Upgrade", "websocket")
                .header("Sec-WebSocket-Version", "13")
                .header(
                    "Sec-WebSocket-Key",
                    tungstenite::handshake::client::generate_key(),
                )
                .body(())
                .map_err(|e| common::Error::WebSocket(format_error_chain(&e)))?;

            match tokio_tungstenite::connect_async(request).await {
                Ok((stream, _)) => stream,
                Err(auth_err) => {
                    let auth_err_msg = format_error_chain(&auth_err);
                    if auth_err_msg.contains("401 Unauthorized") || auth_err_msg.contains("401") {
                        warn!(
                            "Authenticated WebSocket handshake returned 401; retrying without auth headers. \
Check USE_DEMO and API key/secret pairing."
                        );
                        let (stream, _) = tokio_tungstenite::connect_async(self.ws_url.as_str())
                            .await
                            .map_err(|public_err| {
                                common::Error::WebSocket(format!(
                                    "Auth WS connect failed: {}; public fallback failed: {}",
                                    auth_err_msg,
                                    format_error_chain(&public_err),
                                ))
                            })?;
                        stream
                    } else {
                        return Err(common::Error::WebSocket(auth_err_msg));
                    }
                }
            }
        };

        info!("WebSocket connected");

        let (mut write, mut read) = ws_stream.split();
        let mut sub_id = 1u64;
        let mut subscribed_tickers: Vec<String> = Vec::new();
        let mut subscription_poll = tokio::time::interval(Duration::from_secs(2));
        subscription_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        // Process incoming messages and periodically refresh subscription targets.
        loop {
            tokio::select! {
                _ = subscription_poll.tick() => {
                    Self::sync_subscriptions(
                        &mut write,
                        tickers,
                        &mut subscribed_tickers,
                        &mut sub_id
                    ).await?;
                }
                msg_opt = read.next() => {
                    match msg_opt {
                        Some(Ok(tungstenite::Message::Text(text))) => {
                            self.handle_text_message(&text).await;
                        }
                        Some(Ok(tungstenite::Message::Ping(data))) => {
                            let _ = write.send(tungstenite::Message::Pong(data)).await;
                        }
                        Some(Ok(tungstenite::Message::Close(_))) => {
                            info!("WebSocket close frame received");
                            break;
                        }
                        Some(Err(e)) => {
                            return Err(common::Error::WebSocket(format_error_chain(&e)));
                        }
                        None => {
                            warn!("WebSocket stream ended");
                            break;
                        }
                        _ => {}
                    }
                }
            }
        }

        Ok(())
    }

    async fn handle_text_message(&self, text: &str) {
        let msg: WsMessage = match serde_json::from_str(text) {
            Ok(m) => m,
            Err(e) => {
                debug!(
                    "Failed to parse WS message: {} — raw: {}",
                    e,
                    &text[..text.len().min(200)]
                );
                return;
            }
        };

        match msg.msg_type.as_deref() {
            Some("ticker") => {
                if let Some(payload) = msg.msg {
                    match serde_json::from_value::<WsTickerMessage>(payload) {
                        Ok(ticker) => {
                            if !ticker.market_ticker.is_empty() {
                                let mut cache = self.price_cache.write().await;
                                cache.insert(
                                    ticker.market_ticker.clone(),
                                    PriceEntry {
                                        yes_bid: ticker.yes_bid,
                                        yes_ask: ticker.yes_ask,
                                        last_price: ticker.last_price,
                                        volume: ticker.volume,
                                        updated_at: Instant::now(),
                                    },
                                );
                                debug!(
                                    "Ticker: {} — bid={}¢ ask={}¢",
                                    ticker.market_ticker, ticker.yes_bid, ticker.yes_ask,
                                );
                            }
                        }
                        Err(e) => {
                            debug!("Failed to parse ticker payload: {}", e);
                        }
                    }
                }
            }
            Some("error") => {
                warn!("WS error message: {}", text);
            }
            Some(other) => {
                debug!("WS message type '{}' (ignored)", other);
            }
            None => {
                debug!("WS message with no type: {}", &text[..text.len().min(200)]);
            }
        }
    }
}
