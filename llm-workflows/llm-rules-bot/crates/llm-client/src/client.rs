use crate::types::{validate_research_response, ResearchError, ResearchRequest, ResearchResponse};
use reqwest::Client;
use serde_json::json;
use std::time::Duration;
use tokio::time::sleep;
use tracing::instrument;

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";

pub struct LlmClient {
    client: Client,
    api_key: String,
    model: String,
    max_retries: u32,
}

impl LlmClient {
    pub fn new(api_key: String, model: String, timeout_ms: u64, max_retries: u32) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_millis(timeout_ms))
            .build()
            .expect("Failed to build reqwest client");

        Self {
            client,
            api_key,
            model,
            max_retries,
        }
    }

    fn extract_text_content(
        response_body: &serde_json::Value,
    ) -> Result<&str, ResearchError> {
        let content_arr = response_body
            .get("content")
            .and_then(|c| c.as_array())
            .ok_or_else(|| {
                ResearchError::SchemaValidationFailed("Missing or invalid 'content' field".into())
            })?;

        content_arr
            .iter()
            .find(|item| item["type"] == "text")
            .and_then(|item| item["text"].as_str())
            .ok_or_else(|| {
                ResearchError::SchemaValidationFailed("Missing 'text' content".into())
            })
    }

    #[instrument(skip(self, request), fields(request_id = %request.request_id))]
    pub async fn research(&self, request: ResearchRequest) -> Result<ResearchResponse, ResearchError> {
        let schemars_schema = schemars::schema_for!(ResearchResponse);
        let schema_json = serde_json::to_string_pretty(&schemars_schema)
            .map_err(|e| ResearchError::JsonError(e))?;

        let system_prompt = format!(r#"You are a high-precision research assistant for a prediction market trading bot.
Your goal is to analyze market rules and determine the likely resolution outcome or identify risks.
You must output strictly valid JSON conforming to the schema below.
Do NOT output any markdown blocks or conversational text. JUST the JSON object.

JSON Schema:
{}
"#, schema_json);

        let user_prompt = json!({
            "task": "analyze_market_rules",
            "market_ticker": &request.market_ticker,
            "event_ticker": &request.event_ticker,
            "rules": &request.rules_primary,
            "rules_secondary": &request.rules_secondary,
            "context": &request.candidate_snapshot, // Snapshot of current market state
            "allowed_sources": &request.allowed_urls,
            "current_time_ms": request.as_of_ts_ms
        });

        let payload = json!({
            "model": self.model,
            "max_tokens": 1024,
            "system": system_prompt,
            "messages": [
                {
                    "role": "user",
                    "content": serde_json::to_string(&user_prompt)?
                }
            ]
        });

        let mut attempt = 0u32;
        loop {
            let send_result = self
                .client
                .post(ANTHROPIC_API_URL)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", "2023-06-01")
                .header("content-type", "application/json")
                .json(&payload)
                .send()
                .await;

            match send_result {
                Ok(response) => {
                    let status = response.status();
                    if !status.is_success() {
                        let body = response.text().await.unwrap_or_default();
                        if status.as_u16() == 429 && attempt < self.max_retries {
                            attempt += 1;
                            sleep(Duration::from_millis(150 * u64::from(attempt))).await;
                            continue;
                        }
                        return Err(ResearchError::HttpStatus {
                            status: status.as_u16(),
                            body,
                        });
                    }

                    let response_body: serde_json::Value = response
                        .json()
                        .await
                        .map_err(|e| ResearchError::ApiError(e.to_string()))?;
                    let text_content = Self::extract_text_content(&response_body)?;

                    // Parse JSON from text content. Prompt requests JSON-only, but this
                    // remains defensive against occasional wrappers.
                    let json_start = text_content.find('{').unwrap_or(0);
                    let json_end = text_content
                        .rfind('}')
                        .map(|i| i + 1)
                        .unwrap_or(text_content.len());
                    let json_str = &text_content[json_start..json_end];

                    let research_response: ResearchResponse =
                        serde_json::from_str(json_str).map_err(ResearchError::JsonError)?;
                    validate_research_response(&request, &research_response)?;
                    return Ok(research_response);
                }
                Err(e) => {
                    if e.is_timeout() {
                        if attempt < self.max_retries {
                            attempt += 1;
                            sleep(Duration::from_millis(150 * u64::from(attempt))).await;
                            continue;
                        }
                        return Err(ResearchError::Timeout);
                    }
                    if attempt < self.max_retries {
                        attempt += 1;
                        sleep(Duration::from_millis(150 * u64::from(attempt))).await;
                        continue;
                    }
                    return Err(ResearchError::ApiError(e.to_string()));
                }
            }
        }
    }
}
