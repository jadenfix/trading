//! NOAA Weather API client.
//!
//! Fetches gridpoint forecast data from `api.weather.gov` and converts it
//! to probabilities usable by the strategy engine.

pub mod probability;

use chrono::{DateTime, Utc};
use common::config::CityConfig;
use common::{Error, ForecastData};
use serde::Deserialize;
use tracing::{debug, warn};

/// NOAA API client with connection pooling and User-Agent header.
#[derive(Debug, Clone)]
pub struct NoaaClient {
    client: reqwest::Client,
}

// ── NOAA response types ───────────────────────────────────────────────

/// Hourly forecast response from `/gridpoints/{wfo}/{x},{y}/forecast/hourly`.
#[derive(Debug, Deserialize)]
pub struct HourlyForecastResponse {
    pub properties: HourlyForecastProperties,
}

#[derive(Debug, Deserialize)]
pub struct HourlyForecastProperties {
    #[serde(default)]
    pub periods: Vec<ForecastPeriod>,
}

#[derive(Debug, Deserialize)]
pub struct ForecastPeriod {
    pub number: u32,
    #[serde(rename = "startTime")]
    pub start_time: DateTime<Utc>,
    #[serde(rename = "endTime")]
    pub end_time: DateTime<Utc>,
    #[serde(rename = "isDaytime")]
    pub is_daytime: bool,
    pub temperature: serde_json::Value, // can be int or QuantitativeValue
    #[serde(rename = "temperatureUnit", default)]
    pub temperature_unit: Option<String>,
    #[serde(rename = "probabilityOfPrecipitation", default)]
    pub probability_of_precipitation: Option<QuantValue>,
    #[serde(rename = "windSpeed", default)]
    pub wind_speed: Option<serde_json::Value>,
    #[serde(rename = "shortForecast", default)]
    pub short_forecast: String,
}

#[derive(Debug, Deserialize)]
pub struct QuantValue {
    #[serde(rename = "unitCode", default)]
    pub unit_code: String,
    pub value: Option<f64>,
}

/// Raw gridpoint data from `/gridpoints/{wfo}/{x},{y}`.
#[derive(Debug, Deserialize)]
pub struct GridpointResponse {
    pub properties: GridpointProperties,
}

#[derive(Debug, Deserialize)]
pub struct GridpointProperties {
    #[serde(default)]
    pub temperature: Option<GridpointLayer>,
    #[serde(rename = "maxTemperature", default)]
    pub max_temperature: Option<GridpointLayer>,
    #[serde(rename = "minTemperature", default)]
    pub min_temperature: Option<GridpointLayer>,
    #[serde(rename = "probabilityOfPrecipitation", default)]
    pub probability_of_precipitation: Option<GridpointLayer>,
}

#[derive(Debug, Deserialize)]
pub struct GridpointLayer {
    #[serde(default)]
    pub uom: String,
    #[serde(default)]
    pub values: Vec<GridpointValue>,
}

#[derive(Debug, Deserialize)]
pub struct GridpointValue {
    #[serde(rename = "validTime")]
    pub valid_time: String,
    pub value: Option<f64>,
}

// ── Implementation ────────────────────────────────────────────────────

impl NoaaClient {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .user_agent("weather-bot/0.1 (trading bot; contact@example.com)")
            .pool_max_idle_per_host(4)
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("failed to build NOAA HTTP client");

        Self { client }
    }

    /// Fetch the hourly forecast for a city.
    pub async fn fetch_hourly_forecast(
        &self,
        city: &CityConfig,
    ) -> Result<HourlyForecastResponse, Error> {
        let url = format!(
            "https://api.weather.gov/gridpoints/{}/{},{}/forecast/hourly",
            city.wfo, city.grid_x, city.grid_y
        );

        debug!("Fetching NOAA hourly forecast: {}", url);

        let resp = self
            .client
            .get(&url)
            .header("Accept", "application/geo+json")
            .send()
            .await
            .map_err(|e| Error::Noaa(format!("HTTP error for {}: {}", city.name, e)))?;

        let status = resp.status().as_u16();
        if status != 200 {
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Noaa(format!(
                "NOAA returned {} for {}: {}",
                status,
                city.name,
                &body[..body.len().min(500)]
            )));
        }

        let data: HourlyForecastResponse = resp
            .json()
            .await
            .map_err(|e| Error::Noaa(format!("JSON parse error for {}: {}", city.name, e)))?;

        debug!(
            "Got {} forecast periods for {}",
            data.properties.periods.len(),
            city.name
        );

        Ok(data)
    }

    /// Fetch raw gridpoint quantitative data for a city.
    pub async fn fetch_gridpoint_data(
        &self,
        city: &CityConfig,
    ) -> Result<GridpointResponse, Error> {
        let url = format!(
            "https://api.weather.gov/gridpoints/{}/{},{}",
            city.wfo, city.grid_x, city.grid_y
        );

        debug!("Fetching NOAA gridpoint data: {}", url);

        let resp = self
            .client
            .get(&url)
            .header("Accept", "application/geo+json")
            .send()
            .await
            .map_err(|e| Error::Noaa(format!("HTTP error for {}: {}", city.name, e)))?;

        let status = resp.status().as_u16();
        if status != 200 {
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Noaa(format!(
                "NOAA returned {} for {}: {}",
                status,
                city.name,
                &body[..body.len().min(500)]
            )));
        }

        resp.json()
            .await
            .map_err(|e| Error::Noaa(format!("JSON parse error for {}: {}", city.name, e)))
    }

    /// Fetch and process forecast data for a city, returning a `ForecastData`.
    pub async fn get_forecast(&self, city: &CityConfig) -> Result<ForecastData, Error> {
        let hourly = self.fetch_hourly_forecast(city).await?;

        let now = Utc::now();
        let tomorrow_end = now + chrono::Duration::hours(36);

        // Extract temperatures for the next ~24-36 hours (daytime periods).
        let mut temps: Vec<f64> = Vec::new();
        let mut precip_probs: Vec<f64> = Vec::new();

        for period in &hourly.properties.periods {
            if period.start_time > tomorrow_end {
                break;
            }

            // Parse temperature (handles both int and QuantitativeValue).
            let temp = match &period.temperature {
                serde_json::Value::Number(n) => n.as_f64(),
                serde_json::Value::Object(obj) => {
                    obj.get("value").and_then(|v| v.as_f64())
                }
                _ => None,
            };

            if let Some(t) = temp {
                // Convert Celsius to Fahrenheit if needed.
                let temp_f = match period.temperature_unit.as_deref() {
                    Some("C") => t * 9.0 / 5.0 + 32.0,
                    _ => t, // Assume F by default.
                };
                temps.push(temp_f);
            }

            // Extract precipitation probability.
            if let Some(ref precip) = period.probability_of_precipitation {
                if let Some(v) = precip.value {
                    precip_probs.push(v / 100.0);
                }
            }
        }

        if temps.is_empty() {
            return Err(Error::Noaa(format!(
                "No temperature data found for {}",
                city.name
            )));
        }

        let high = temps.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let low = temps.iter().cloned().fold(f64::INFINITY, f64::min);
        let avg_precip = if precip_probs.is_empty() {
            0.0
        } else {
            precip_probs.iter().sum::<f64>() / precip_probs.len() as f64
        };

        // Estimate temperature uncertainty from hourly spread.
        let mean_temp = temps.iter().sum::<f64>() / temps.len() as f64;
        let variance = temps.iter().map(|t| (t - mean_temp).powi(2)).sum::<f64>() / temps.len() as f64;
        let std_dev = variance.sqrt().max(2.0); // Minimum 2°F uncertainty.

        debug!(
            "{}: high={:.1}°F low={:.1}°F precip={:.0}% uncertainty=±{:.1}°F",
            city.name, high, low, avg_precip * 100.0, std_dev
        );

        Ok(ForecastData {
            city: city.name.clone(),
            high_temp_f: high,
            low_temp_f: low,
            precip_prob: avg_precip,
            temp_std_dev: std_dev,
            fetched_at: Utc::now(),
        })
    }
}

impl Default for NoaaClient {
    fn default() -> Self {
        Self::new()
    }
}
