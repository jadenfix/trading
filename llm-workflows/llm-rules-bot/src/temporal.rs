use std::time::Duration;

use llm_client::{
    validate_research_response, ResearchError, ResearchRequest, ResearchResponse,
};
use reqwest::Client;
use serde_json::{json, Value};
use tokio::time::{sleep, Instant};

use crate::config::TemporalConfig;

pub struct TemporalResearchClient {
    client: Client,
    base_url: String,
    start_path: String,
    poll_path_template: String,
    poll_interval_ms: u64,
    timeout_ms: u64,
    auth_token: Option<String>,
}

impl TemporalResearchClient {
    pub fn new(config: &TemporalConfig) -> anyhow::Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_millis(config.request_timeout_ms))
            .build()?;

        let auth_token = if config.auth_token_env.trim().is_empty() {
            None
        } else {
            std::env::var(config.auth_token_env.trim()).ok()
        };

        Ok(Self {
            client,
            base_url: config.broker_base_url.trim_end_matches('/').to_string(),
            start_path: config.start_path.clone(),
            poll_path_template: config.poll_path_template.clone(),
            poll_interval_ms: config.poll_interval_ms,
            timeout_ms: config.workflow_timeout_ms,
            auth_token,
        })
    }

    fn build_url(&self, path: &str) -> String {
        format!("{}/{}", self.base_url, path.trim_start_matches('/'))
    }

    fn add_auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(token) = &self.auth_token {
            req.bearer_auth(token)
        } else {
            req
        }
    }

    fn parse_workflow_id(value: &Value) -> Option<String> {
        [
            "workflow_id",
            "run_id",
            "job_id",
            "id",
            "research_id",
            "request_id",
        ]
        .iter()
        .find_map(|k| value.get(*k).and_then(|v| v.as_str()).map(ToString::to_string))
    }

    fn parse_status(value: &Value) -> Option<String> {
        value
            .get("status")
            .or_else(|| value.get("state"))
            .or_else(|| value.get("phase"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_ascii_lowercase())
    }

    fn parse_result(value: &Value) -> Option<Value> {
        value
            .get("result")
            .or_else(|| value.get("response"))
            .or_else(|| value.get("research_response"))
            .or_else(|| value.get("output"))
            .cloned()
    }

    async fn start_workflow(
        &self,
        request: &ResearchRequest,
    ) -> Result<(String, Option<ResearchResponse>), ResearchError> {
        let payload = json!({
            "request_id": request.request_id.to_string(),
            "workflow_type": "ResearchOpportunityWorkflow",
            "input": request,
        });

        let response = self
            .add_auth(self.client.post(self.build_url(&self.start_path)))
            .json(&payload)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ResearchError::Timeout
                } else {
                    ResearchError::ApiError(e.to_string())
                }
            })?;

        let status = response.status();
        if !status.is_success() {
            return Err(ResearchError::HttpStatus {
                status: status.as_u16(),
                body: response.text().await.unwrap_or_default(),
            });
        }

        let body: Value = response
            .json()
            .await
            .map_err(|e| ResearchError::ApiError(e.to_string()))?;
        let workflow_id = Self::parse_workflow_id(&body).ok_or_else(|| {
            ResearchError::SchemaValidationFailed(
                "temporal start response missing workflow id".into(),
            )
        })?;

        let immediate = Self::parse_result(&body)
            .and_then(|result| serde_json::from_value::<ResearchResponse>(result).ok());

        Ok((workflow_id, immediate))
    }

    async fn poll_once(&self, workflow_id: &str) -> Result<Value, ResearchError> {
        let path = self.poll_path_template.replace("{id}", workflow_id);
        let response = self
            .add_auth(self.client.get(self.build_url(&path)))
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ResearchError::Timeout
                } else {
                    ResearchError::ApiError(e.to_string())
                }
            })?;

        let status = response.status();
        if !status.is_success() {
            return Err(ResearchError::HttpStatus {
                status: status.as_u16(),
                body: response.text().await.unwrap_or_default(),
            });
        }

        response
            .json()
            .await
            .map_err(|e| ResearchError::ApiError(e.to_string()))
    }

    pub async fn research(
        &self,
        request: ResearchRequest,
    ) -> Result<(String, ResearchResponse), ResearchError> {
        let (workflow_id, immediate) = self.start_workflow(&request).await?;
        if let Some(response) = immediate {
            validate_research_response(&request, &response)?;
            return Ok((workflow_id, response));
        }

        let deadline = Instant::now() + Duration::from_millis(self.timeout_ms);
        loop {
            if Instant::now() >= deadline {
                return Err(ResearchError::Timeout);
            }

            let payload = self.poll_once(&workflow_id).await?;
            if let Some(result) = Self::parse_result(&payload) {
                let response: ResearchResponse =
                    serde_json::from_value(result).map_err(ResearchError::JsonError)?;
                validate_research_response(&request, &response)?;
                return Ok((workflow_id, response));
            }

            let status = Self::parse_status(&payload).unwrap_or_else(|| "unknown".into());
            if matches!(status.as_str(), "failed" | "error" | "terminated" | "canceled") {
                return Err(ResearchError::ApiError(format!(
                    "temporal workflow {} ended with status={}",
                    workflow_id, status
                )));
            }

            sleep(Duration::from_millis(self.poll_interval_ms)).await;
        }
    }
}
