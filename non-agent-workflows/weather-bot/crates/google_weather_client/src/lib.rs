//! Google Weather API client.
//!
//! Fetches hourly forecast data from Google Maps Platform Weather API
//! and converts it to the shared `ForecastData` format.

use chrono::{DateTime, Utc};
use common::config::CityConfig;
use common::{Error, ForecastData};
use serde::Deserialize;
use tracing::debug;

const LOOKUP_URL: &str = "https://weather.googleapis.com/v1/forecast/hours:lookup";
const DEFAULT_WINDOW_HOURS: usize = 36;
const MAX_PAGE_SIZE: usize = 24;

/// Google Weather API client.
#[derive(Debug, Clone)]
pub struct GoogleWeatherClient {
    client: reqwest::Client,
    api_key: String,
}

/// Response from `forecast/hours:lookup`.
#[derive(Debug, Deserialize)]
pub struct LookupForecastHoursResponse {
    #[serde(rename = "forecastHours", default)]
    pub forecast_hours: Vec<ForecastHour>,
    #[serde(rename = "nextPageToken", default)]
    pub next_page_token: Option<String>,
}

/// Forecast for a single hour.
#[derive(Debug, Clone, Deserialize)]
pub struct ForecastHour {
    #[serde(default)]
    pub interval: Option<Interval>,
    #[serde(default)]
    pub temperature: Option<Temperature>,
    #[serde(default)]
    pub precipitation: Option<Precipitation>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Interval {
    #[serde(rename = "startTime", default)]
    pub start_time: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Temperature {
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub degrees: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Precipitation {
    #[serde(default)]
    pub probability: Option<PrecipitationProbability>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PrecipitationProbability {
    #[serde(default)]
    pub percent: Option<f64>,
}

impl GoogleWeatherClient {
    pub fn new(api_key: String) -> Self {
        let client = reqwest::Client::builder()
            .user_agent("weather-bot/0.1 (trading bot; contact@example.com)")
            .pool_max_idle_per_host(4)
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("failed to build Google Weather HTTP client");

        Self { client, api_key }
    }

    /// Fetch hourly forecast rows for the next ~36 hours.
    pub async fn fetch_hourly_forecast(
        &self,
        lat: f64,
        lon: f64,
    ) -> Result<Vec<ForecastHour>, Error> {
        let mut collected: Vec<ForecastHour> = Vec::new();
        let mut page_token: Option<String> = None;

        while collected.len() < DEFAULT_WINDOW_HOURS {
            let mut query = vec![
                ("location.latitude", lat.to_string()),
                ("location.longitude", lon.to_string()),
                ("unitsSystem", "IMPERIAL".to_string()),
                ("hours", DEFAULT_WINDOW_HOURS.to_string()),
                ("pageSize", MAX_PAGE_SIZE.to_string()),
                ("key", self.api_key.clone()),
            ];
            if let Some(token) = &page_token {
                query.push(("pageToken", token.clone()));
            }

            debug!(
                "Fetching Google Weather hourly forecast: {} lat={} lon={} page_token_present={}",
                LOOKUP_URL,
                lat,
                lon,
                page_token.is_some()
            );

            let resp = self
                .client
                .get(LOOKUP_URL)
                .query(&query)
                .send()
                .await
                .map_err(|e| Error::GoogleWeather(format!("HTTP error for ({lat},{lon}): {e}")))?;

            let status = resp.status().as_u16();
            if status != 200 {
                let body = resp.text().await.unwrap_or_default();
                return Err(Error::GoogleWeather(format!(
                    "Google Weather returned {} for ({lat},{lon}): {}",
                    status,
                    &body[..body.len().min(500)]
                )));
            }

            let payload: LookupForecastHoursResponse = resp.json().await.map_err(|e| {
                Error::GoogleWeather(format!("JSON parse error for ({lat},{lon}): {e}"))
            })?;

            if payload.forecast_hours.is_empty() {
                break;
            }

            collected.extend(payload.forecast_hours);
            page_token = payload.next_page_token;
            if page_token.is_none() {
                break;
            }
        }

        if collected.is_empty() {
            return Err(Error::GoogleWeather(format!(
                "No hourly forecast rows for ({lat},{lon})"
            )));
        }

        Ok(collected)
    }

    /// Fetch and process forecast data for a city, returning `ForecastData`.
    pub async fn get_forecast(&self, city: &CityConfig) -> Result<ForecastData, Error> {
        let hours = self.fetch_hourly_forecast(city.lat, city.lon).await?;
        summarize_hours(&city.name, Utc::now(), &hours)
    }
}

fn parse_start_time(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

fn extract_temp_f(hour: &ForecastHour) -> Option<f64> {
    let temp = hour.temperature.as_ref()?;
    let degrees = temp.degrees?;

    match temp.unit.as_deref() {
        Some("CELSIUS") => Some(degrees * 9.0 / 5.0 + 32.0),
        _ => Some(degrees),
    }
}

fn extract_precip_prob(hour: &ForecastHour) -> Option<f64> {
    hour.precipitation
        .as_ref()?
        .probability
        .as_ref()?
        .percent
        .map(|p| (p / 100.0).clamp(0.0, 1.0))
}

fn summarize_hours(
    city_name: &str,
    now: DateTime<Utc>,
    hours: &[ForecastHour],
) -> Result<ForecastData, Error> {
    let horizon = now + chrono::Duration::hours(DEFAULT_WINDOW_HOURS as i64);

    let mut temps: Vec<f64> = Vec::new();
    let mut precip_probs: Vec<f64> = Vec::new();

    for hour in hours {
        let start_time = hour
            .interval
            .as_ref()
            .and_then(|interval| interval.start_time.as_deref())
            .and_then(parse_start_time);

        let Some(start_time) = start_time else {
            continue;
        };
        if start_time < now || start_time > horizon {
            continue;
        }

        if let Some(temp_f) = extract_temp_f(hour) {
            temps.push(temp_f);
        }
        if let Some(prob) = extract_precip_prob(hour) {
            precip_probs.push(prob);
        }
    }

    if temps.is_empty() {
        return Err(Error::GoogleWeather(format!(
            "No temperature data found for {}",
            city_name
        )));
    }

    let high = temps.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let low = temps.iter().copied().fold(f64::INFINITY, f64::min);
    let avg_precip = if precip_probs.is_empty() {
        0.0
    } else {
        precip_probs.iter().sum::<f64>() / precip_probs.len() as f64
    };

    let mean_temp = temps.iter().sum::<f64>() / temps.len() as f64;
    let variance = temps.iter().map(|t| (t - mean_temp).powi(2)).sum::<f64>() / temps.len() as f64;
    let std_dev = variance.sqrt().max(2.0);

    Ok(ForecastData {
        city: city_name.to_string(),
        high_temp_f: high,
        low_temp_f: low,
        precip_prob: avg_precip,
        temp_std_dev: std_dev,
        fetched_at: Utc::now(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_response() -> &'static str {
        r#"{
            "forecastHours": [
                {
                    "interval": {"startTime": "2026-02-13T10:00:00Z", "endTime": "2026-02-13T11:00:00Z"},
                    "temperature": {"unit": "CELSIUS", "degrees": 25},
                    "precipitation": {"probability": {"percent": 30}}
                },
                {
                    "interval": {"startTime": "2026-02-13T11:00:00Z", "endTime": "2026-02-13T12:00:00Z"},
                    "temperature": {"unit": "FAHRENHEIT", "degrees": 78},
                    "precipitation": {"probability": {"percent": 10}}
                },
                {
                    "interval": {"startTime": "2026-02-13T12:00:00Z", "endTime": "2026-02-13T13:00:00Z"},
                    "temperature": {"unit": "FAHRENHEIT", "degrees": 74},
                    "precipitation": {"probability": {"percent": 20}}
                }
            ],
            "nextPageToken": "token-2"
        }"#
    }

    #[test]
    fn test_deserialize_lookup_response() {
        let parsed: LookupForecastHoursResponse =
            serde_json::from_str(sample_response()).expect("response should deserialize");

        assert_eq!(parsed.forecast_hours.len(), 3);
        assert_eq!(parsed.next_page_token.as_deref(), Some("token-2"));
    }

    #[test]
    fn test_summarize_hours_maps_to_forecast_data() {
        let parsed: LookupForecastHoursResponse =
            serde_json::from_str(sample_response()).expect("response should deserialize");
        let now = DateTime::parse_from_rfc3339("2026-02-13T09:00:00Z")
            .expect("valid now")
            .with_timezone(&Utc);

        let forecast =
            summarize_hours("Seattle", now, &parsed.forecast_hours).expect("summary should build");

        assert_eq!(forecast.city, "Seattle");
        assert!((forecast.high_temp_f - 78.0).abs() < 0.01);
        assert!((forecast.low_temp_f - 74.0).abs() < 0.01);
        assert!((forecast.precip_prob - 0.2).abs() < 0.0001);
        assert!(forecast.temp_std_dev >= 2.0);
    }
}
