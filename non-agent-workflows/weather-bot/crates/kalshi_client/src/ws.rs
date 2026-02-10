//! WebSocket client for the Kalshi ticker feed.
//!
//! Connects to the public `ticker` channel via
//! `wss://api.elections.kalshi.com/trade-api/ws/v2` (or demo equivalent),
//! and streams price updates into a shared `PriceCache`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{WsMessage, WsSubscribeCmd, WsSubscribeParams, WsTickerMessage};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::RwLock;
use tokio::time::sleep;
use tokio_tungstenite::tungstenite;
use tracing::{debug, error, info, warn};

use crate::auth::KalshiAuth;

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
    pub fn new(auth: KalshiAuth, use_demo: bool, price_cache: PriceCache) -> Self {
        let ws_url = if use_demo {
            "wss://demo-api.kalshi.co/trade-api/ws/v2".to_string()
        } else {
            "wss://api.elections.kalshi.com/trade-api/ws/v2".to_string()
        };

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
                    error!(
                        "WebSocket error: {}. Reconnecting in {:?}",
                        e, backoff
                    );
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
        // Build auth headers for the WebSocket upgrade.
        let ws_path = "/trade-api/ws/v2";
        let (timestamp, signature) = self.auth.sign_request("GET", ws_path);

        let url = url::Url::parse(&self.ws_url)
            .map_err(|e| common::Error::WebSocket(e.to_string()))?;

        let request = tungstenite::http::Request::builder()
            .uri(self.ws_url.as_str())
            .header("KALSHI-ACCESS-KEY", &self.auth.api_key)
            .header("KALSHI-ACCESS-TIMESTAMP", &timestamp)
            .header("KALSHI-ACCESS-SIGNATURE", &signature)
            .header("Host", url.host_str().unwrap_or("api.elections.kalshi.com"))
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header(
                "Sec-WebSocket-Key",
                tungstenite::handshake::client::generate_key(),
            )
            .body(())
            .map_err(|e| common::Error::WebSocket(e.to_string()))?;

        let (ws_stream, _) =
            tokio_tungstenite::connect_async(request)
                .await
                .map_err(|e| common::Error::WebSocket(e.to_string()))?;

        info!("WebSocket connected");

        let (mut write, mut read) = ws_stream.split();

        // Subscribe to ticker channel for our markets.
        let ticker_list = tickers.read().await.clone();
        if !ticker_list.is_empty() {
            let sub = WsSubscribeCmd {
                id: 1,
                cmd: "subscribe".to_string(),
                params: WsSubscribeParams {
                    channels: vec!["ticker".to_string()],
                    market_tickers: Some(ticker_list.clone()),
                },
            };

            let sub_json = serde_json::to_string(&sub)
                .map_err(|e| common::Error::WebSocket(e.to_string()))?;

            write
                .send(tungstenite::Message::Text(sub_json.into()))
                .await
                .map_err(|e| common::Error::WebSocket(e.to_string()))?;

            info!("Subscribed to {} tickers", ticker_list.len());
        } else {
            warn!("No tickers to subscribe to yet");
        }

        // Process incoming messages.
        while let Some(msg_result) = read.next().await {
            match msg_result {
                Ok(tungstenite::Message::Text(text)) => {
                    self.handle_text_message(&text).await;
                }
                Ok(tungstenite::Message::Ping(data)) => {
                    let _ = write
                        .send(tungstenite::Message::Pong(data))
                        .await;
                }
                Ok(tungstenite::Message::Close(_)) => {
                    info!("WebSocket close frame received");
                    break;
                }
                Err(e) => {
                    return Err(common::Error::WebSocket(e.to_string()));
                }
                _ => {}
            }
        }

        Ok(())
    }

    async fn handle_text_message(&self, text: &str) {
        let msg: WsMessage = match serde_json::from_str(text) {
            Ok(m) => m,
            Err(e) => {
                debug!("Failed to parse WS message: {} — raw: {}", e, &text[..text.len().min(200)]);
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
