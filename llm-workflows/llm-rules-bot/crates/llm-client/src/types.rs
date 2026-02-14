use serde::{Deserialize, Serialize};
use schemars::JsonSchema;
use std::collections::HashSet;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ResearchRequest {
    pub request_id: Uuid,
    pub market_ticker: String,
    pub event_ticker: String,
    pub rules_primary: String,
    pub rules_secondary: Option<String>,
    // This will be a JSON object representing the candidate state
    pub candidate_snapshot: serde_json::Value, 
    pub allowed_urls: Vec<String>,
    pub as_of_ts_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResearchResponse {
    pub resolution_source: String,
    pub definition_summary: String,
    pub edge_case_flags: Vec<String>,
    pub risk_of_misresolution: RiskLevel,
    pub fact_claims: Vec<FactClaim>,
    pub confidence: f64, // 0.0 to 1.0
    pub uncertainty: f64, // 0.0 to 1.0
    pub as_of_ts_ms: i64,
    pub provenance: Vec<Provenance>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FactClaim {
    pub claim: String,
    pub source_url: Option<String>,
    pub verifiability_score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Provenance {
    pub url: String,
    pub title: Option<String>,
    pub fetched_at_ts_ms: i64,
}

#[derive(Debug, thiserror::Error)]
pub enum ResearchError {
    #[error("API request failed: {0}")]
    ApiError(String),
    #[error("HTTP status {status}: {body}")]
    HttpStatus { status: u16, body: String },
    #[error("JSON parsing failed: {0}")]
    JsonError(#[from] serde_json::Error),
    #[error("Timeout")]
    Timeout,
    #[error("Schema validation failed: {0}")]
    SchemaValidationFailed(String),
}

pub fn validate_research_response(
    request: &ResearchRequest,
    response: &ResearchResponse,
) -> Result<(), ResearchError> {
    if !(0.0..=1.0).contains(&response.confidence) {
        return Err(ResearchError::SchemaValidationFailed(
            "confidence must be in [0,1]".into(),
        ));
    }
    if !(0.0..=1.0).contains(&response.uncertainty) {
        return Err(ResearchError::SchemaValidationFailed(
            "uncertainty must be in [0,1]".into(),
        ));
    }
    if response.as_of_ts_ms <= 0 {
        return Err(ResearchError::SchemaValidationFailed(
            "as_of_ts_ms must be positive".into(),
        ));
    }
    if !response.fact_claims.is_empty() && response.provenance.is_empty() {
        return Err(ResearchError::SchemaValidationFailed(
            "fact_claims require provenance".into(),
        ));
    }

    for claim in &response.fact_claims {
        if !(0.0..=1.0).contains(&claim.verifiability_score) {
            return Err(ResearchError::SchemaValidationFailed(format!(
                "verifiability_score out of range for claim: {}",
                claim.claim
            )));
        }
    }

    let provenance_urls: HashSet<&str> = response.provenance.iter().map(|p| p.url.as_str()).collect();
    for claim in &response.fact_claims {
        if let Some(url) = claim.source_url.as_deref() {
            if !provenance_urls.contains(url) {
                return Err(ResearchError::SchemaValidationFailed(format!(
                    "claim source_url missing from provenance: {}",
                    url
                )));
            }

            if !request.allowed_urls.is_empty()
                && !request.allowed_urls.iter().any(|allowed| url.starts_with(allowed))
            {
                return Err(ResearchError::SchemaValidationFailed(format!(
                    "claim source_url not whitelisted: {}",
                    url
                )));
            }
        }
    }

    Ok(())
}
