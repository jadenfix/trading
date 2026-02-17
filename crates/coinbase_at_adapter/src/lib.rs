use std::collections::HashMap;
use std::env;
use std::sync::Arc;

use exchange_core::{
    BalanceSnapshot, ExchangeAdapter, ExchangeError, ExchangeHealth, ExchangeResultFuture,
    ExchangeValueFuture, FillReport, InstrumentType, NormalizedOrderRequest, OpenOrderSnapshot,
    OrderAck, OrderSide, OrderSnapshot, OrderStatus, OrderType, PositionSnapshot,
};
use reqwest::{header, Method};
use serde_json::{json, Value};
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct CoinbaseAdvancedTradeAdapter {
    api_base: String,
    http_client: reqwest::Client,
    bearer_token: String,
    orders: Arc<Mutex<HashMap<String, OrderSnapshot>>>,
    fills: Arc<Mutex<Vec<FillReport>>>,
}

impl CoinbaseAdvancedTradeAdapter {
    pub fn from_env() -> Result<Self, ExchangeError> {
        let bearer_token = env::var("COINBASE_BEARER_TOKEN").map_err(|_| {
            ExchangeError::new(
                "missing_credentials",
                "COINBASE_BEARER_TOKEN is required",
                false,
            )
        })?;
        let api_base = env::var("COINBASE_API_BASE_URL")
            .unwrap_or_else(|_| "https://api.coinbase.com".to_string())
            .trim_end_matches('/')
            .to_string();
        let http_client = reqwest::Client::builder().build().map_err(|e| {
            ExchangeError::new(
                "http_client_init",
                format!("failed to initialize HTTP client: {e}"),
                false,
            )
        })?;

        Ok(Self {
            api_base,
            http_client,
            bearer_token,
            orders: Arc::new(Mutex::new(HashMap::new())),
            fills: Arc::new(Mutex::new(Vec::new())),
        })
    }

    pub fn credentials_present() -> bool {
        env::var("COINBASE_BEARER_TOKEN").is_ok()
    }

    fn now_ms() -> i64 {
        chrono::Utc::now().timestamp_millis()
    }

    async fn run_http_json(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, ExchangeError> {
        let method = Method::from_bytes(method.as_bytes()).map_err(|e| {
            ExchangeError::new(
                "invalid_http_method",
                format!("invalid HTTP method `{method}`: {e}"),
                false,
            )
        })?;
        let mut request = self
            .http_client
            .request(method, format!("{}{}", self.api_base, path))
            .header(header::CONTENT_TYPE, "application/json")
            .bearer_auth(&self.bearer_token);

        if let Some(body) = body {
            request = request.json(body);
        }

        let response = request.send().await.map_err(|e| {
            ExchangeError::new("venue_http_send", format!("request failed: {e}"), true)
        })?;
        let status = response.status();
        let response_text = response.text().await.map_err(|e| {
            ExchangeError::new(
                "venue_http_read",
                format!("failed to read response body: {e}"),
                true,
            )
        })?;

        if !status.is_success() {
            return Err(ExchangeError::new(
                "venue_http_error",
                format!(
                    "coinbase_at returned status={} body={}",
                    status,
                    response_text.trim()
                ),
                status.is_server_error(),
            ));
        }

        if response_text.trim().is_empty() {
            return Ok(json!({}));
        }

        serde_json::from_str::<Value>(&response_text).map_err(|e| {
            ExchangeError::new(
                "json_decode",
                format!(
                    "invalid venue json response: {e}; body={}",
                    response_text.trim()
                ),
                false,
            )
        })
    }

    fn extract_order_id(payload: &Value) -> Option<String> {
        payload
            .get("success_response")
            .and_then(|v| v.get("order_id"))
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .or_else(|| {
                payload
                    .get("order_id")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            })
    }

    fn extract_order_error(payload: &Value) -> Option<String> {
        payload
            .get("error_response")
            .and_then(|error| {
                error
                    .get("message")
                    .or_else(|| error.get("error"))
                    .or_else(|| error.get("reason"))
                    .and_then(Value::as_str)
            })
            .map(ToString::to_string)
            .or_else(|| {
                payload
                    .get("message")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            })
            .or_else(|| {
                payload
                    .get("error")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            })
    }

    fn parse_order_id(payload: &Value) -> Result<String, ExchangeError> {
        if let Some(order_id) = Self::extract_order_id(payload) {
            return Ok(order_id);
        }
        if let Some(message) = Self::extract_order_error(payload) {
            return Err(ExchangeError::new(
                "order_rejected",
                format!("coinbase_at rejected order: {message}"),
                false,
            ));
        }
        Err(ExchangeError::new(
            "missing_order_id",
            format!("coinbase_at response missing order_id: {payload}"),
            false,
        ))
    }
}

impl ExchangeAdapter for CoinbaseAdvancedTradeAdapter {
    fn venue(&self) -> &'static str {
        "coinbase_at"
    }

    fn connect_market_data(&self) -> ExchangeResultFuture<'_, ()> {
        Box::pin(async move {
            self.run_http_json("GET", "/api/v3/brokerage/time", None)
                .await
                .map(|_| ())
        })
    }

    fn place_order(&self, req: NormalizedOrderRequest) -> ExchangeResultFuture<'_, OrderAck> {
        Box::pin(async move {
            if req.instrument.instrument_type != InstrumentType::Spot {
                return Err(ExchangeError::new(
                    "unsupported_instrument",
                    "coinbase_at adapter supports spot only in v1",
                    false,
                ));
            }

            let order_configuration = if req.order_type == OrderType::Market {
                json!({
                    "market_market_ioc": {
                        "base_size": format!("{}", req.qty)
                    }
                })
            } else {
                let limit_price = req.limit_price.ok_or_else(|| {
                    ExchangeError::new(
                        "missing_limit_price",
                        "limit_price required for limit orders",
                        false,
                    )
                })?;

                json!({
                    "limit_limit_gtc": {
                        "base_size": format!("{}", req.qty),
                        "limit_price": format!("{}", limit_price),
                        "post_only": req.post_only
                    }
                })
            };

            let body = json!({
                "client_order_id": req.client_order_id,
                "product_id": req.instrument.venue_symbol,
                "side": if req.side == OrderSide::Buy { "BUY" } else { "SELL" },
                "order_configuration": order_configuration,
            });

            let response = self
                .run_http_json("POST", "/api/v3/brokerage/orders", Some(&body))
                .await?;

            let now = Self::now_ms();
            let venue_order_id = Self::parse_order_id(&response)?;

            let snapshot = OrderSnapshot {
                venue: self.venue().to_string(),
                venue_order_id: venue_order_id.clone(),
                client_order_id: req.client_order_id.clone(),
                strategy_id: req.strategy_id.clone(),
                instrument: req.instrument,
                side: req.side,
                order_type: req.order_type,
                status: OrderStatus::New,
                qty: req.qty,
                filled_qty: 0.0,
                limit_price: req.limit_price,
                avg_fill_price: None,
                created_at_ms: now,
                updated_at_ms: now,
                simulated: false,
            };

            self.orders
                .lock()
                .await
                .insert(venue_order_id.clone(), snapshot);

            Ok(OrderAck {
                venue_order_id,
                client_order_id: req.client_order_id,
                accepted: true,
                status: OrderStatus::New,
                filled_qty: 0.0,
                avg_fill_price: None,
                simulated: false,
                reason: None,
                ts_ms: now,
            })
        })
    }

    fn cancel_order(&self, venue_order_id: &str) -> ExchangeResultFuture<'_, ()> {
        let venue_order_id = venue_order_id.to_string();
        Box::pin(async move {
            let body = json!({ "order_ids": [venue_order_id.clone()] });
            self.run_http_json("POST", "/api/v3/brokerage/orders/batch_cancel", Some(&body))
                .await?;

            if let Some(order) = self.orders.lock().await.get_mut(&venue_order_id) {
                order.status = OrderStatus::Canceled;
                order.updated_at_ms = Self::now_ms();
            }

            Ok(())
        })
    }

    fn get_order(&self, venue_order_id: &str) -> ExchangeResultFuture<'_, Option<OrderSnapshot>> {
        let venue_order_id = venue_order_id.to_string();
        Box::pin(async move { Ok(self.orders.lock().await.get(&venue_order_id).cloned()) })
    }

    fn open_orders(&self) -> ExchangeResultFuture<'_, Vec<OpenOrderSnapshot>> {
        Box::pin(async move {
            let orders = self.orders.lock().await;
            Ok(orders
                .values()
                .filter(|order| {
                    matches!(
                        order.status,
                        OrderStatus::New | OrderStatus::PartiallyFilled
                    )
                })
                .cloned()
                .map(|order| OpenOrderSnapshot { order })
                .collect())
        })
    }

    fn fills_since(
        &self,
        since_ts_ms: i64,
        limit: usize,
    ) -> ExchangeResultFuture<'_, Vec<FillReport>> {
        Box::pin(async move {
            let fills = self.fills.lock().await;
            let mut filtered: Vec<FillReport> = fills
                .iter()
                .filter(|fill| fill.ts_ms >= since_ts_ms)
                .cloned()
                .collect();
            filtered.sort_by_key(|fill| fill.ts_ms);
            if filtered.len() > limit {
                filtered = filtered[filtered.len().saturating_sub(limit)..].to_vec();
            }
            Ok(filtered)
        })
    }

    fn sync_positions(&self) -> ExchangeResultFuture<'_, Vec<PositionSnapshot>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn sync_balances(&self) -> ExchangeResultFuture<'_, Vec<BalanceSnapshot>> {
        Box::pin(async move {
            let payload = self
                .run_http_json("GET", "/api/v3/brokerage/accounts", None)
                .await?;

            let mut balances = Vec::new();
            if let Some(accounts) = payload.get("accounts").and_then(Value::as_array) {
                for account in accounts {
                    let currency = account
                        .get("currency")
                        .and_then(Value::as_str)
                        .unwrap_or("UNKNOWN")
                        .to_string();
                    let available = account
                        .get("available_balance")
                        .and_then(|v| v.get("value"))
                        .and_then(Value::as_str)
                        .and_then(|v| v.parse::<f64>().ok())
                        .unwrap_or(0.0);
                    let hold = account
                        .get("hold")
                        .and_then(|v| v.get("value"))
                        .and_then(Value::as_str)
                        .and_then(|v| v.parse::<f64>().ok())
                        .unwrap_or(0.0);
                    balances.push(BalanceSnapshot {
                        venue: self.venue().to_string(),
                        asset: currency,
                        total: available + hold,
                        available,
                    });
                }
            }

            Ok(balances)
        })
    }

    fn health(&self) -> ExchangeValueFuture<'_, ExchangeHealth> {
        Box::pin(async move {
            let healthy = self
                .run_http_json("GET", "/api/v3/brokerage/time", None)
                .await
                .is_ok();

            ExchangeHealth {
                venue: self.venue().to_string(),
                healthy,
                connected_market_data: healthy,
                connected_trading: healthy,
                message: if healthy {
                    Some("coinbase_at reachable".to_string())
                } else {
                    Some("coinbase_at health probe failed".to_string())
                },
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_order_id_accepts_success_response_shape() {
        let payload = json!({
            "success_response": {
                "order_id": "abc-123"
            }
        });
        let order_id = CoinbaseAdvancedTradeAdapter::parse_order_id(&payload)
            .expect("order id should be extracted");
        assert_eq!(order_id, "abc-123");
    }

    #[test]
    fn parse_order_id_surfaces_venue_rejection() {
        let payload = json!({
            "error_response": {
                "message": "INSUFFICIENT_FUND"
            }
        });
        let err = CoinbaseAdvancedTradeAdapter::parse_order_id(&payload)
            .expect_err("expected rejection error");
        assert_eq!(err.code, "order_rejected");
        assert!(err.message.contains("INSUFFICIENT_FUND"));
    }

    #[test]
    fn parse_order_id_fails_when_response_shape_is_unknown() {
        let payload = json!({ "success": true });
        let err = CoinbaseAdvancedTradeAdapter::parse_order_id(&payload)
            .expect_err("missing order id must fail");
        assert_eq!(err.code, "missing_order_id");
    }
}
