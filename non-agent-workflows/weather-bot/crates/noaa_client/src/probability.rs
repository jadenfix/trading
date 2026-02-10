//! Forecast-to-probability conversion.
//!
//! Converts NOAA temperature forecasts into probabilities for
//! Kalshi weather market settlement.

use common::ForecastData;

/// Compute the probability that a weather market settles YES,
/// given the forecast data and the market's strike parameters.
///
/// Assumes a normal distribution around the forecast high/low with
/// the forecast's own uncertainty (std dev).
///
/// # Strike types
/// - `"greater"`: settles YES if actual temp > strike
/// - `"less"`:    settles YES if actual temp < strike
/// - `"between"`: settles YES if floor_strike <= temp <= cap_strike
pub fn compute_p_yes(
    forecast: &ForecastData,
    strike_type: &str,
    floor_strike: Option<i64>,
    cap_strike: Option<i64>,
) -> f64 {
    let std_dev = forecast.temp_std_dev.max(2.0);

    match strike_type {
        "greater" | "greater_or_equal" => {
            // P(high_temp >= strike)
            let strike = floor_strike.unwrap_or(0) as f64;
            let z = (forecast.high_temp_f - strike) / std_dev;
            normal_cdf(z)
        }
        "less" | "less_or_equal" => {
            // P(low_temp <= strike)
            let strike = cap_strike.unwrap_or(100) as f64;
            let z = (strike - forecast.low_temp_f) / std_dev;
            normal_cdf(z)
        }
        "between" => {
            // P(floor <= temp <= cap)
            let floor = floor_strike.unwrap_or(0) as f64;
            let cap = cap_strike.unwrap_or(100) as f64;
            let mean = forecast.high_temp_f;

            let z_high = (cap - mean) / std_dev;
            let z_low = (floor - mean) / std_dev;

            (normal_cdf(z_high) - normal_cdf(z_low)).max(0.0)
        }
        _ => {
            // Unknown strike type — conservative 0.5.
            0.5
        }
    }
}

/// Approximate normal CDF using the logistic approximation.
/// Accurate to ~0.01 for practical ranges.
fn normal_cdf(z: f64) -> f64 {
    // Clamp to avoid overflow.
    let z = z.clamp(-10.0, 10.0);
    1.0 / (1.0 + (-1.7155277699 * z).exp())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_forecast(high: f64, low: f64, std_dev: f64) -> ForecastData {
        ForecastData {
            city: "Test".into(),
            high_temp_f: high,
            low_temp_f: low,
            precip_prob: 0.0,
            temp_std_dev: std_dev,
            fetched_at: Utc::now(),
        }
    }

    #[test]
    fn test_p_yes_far_above_strike() {
        // Forecast high = 80°F, strike = 50°F, std_dev = 3°F → very likely YES
        let fc = make_forecast(80.0, 60.0, 3.0);
        let p = compute_p_yes(&fc, "greater", Some(50), None);
        assert!(p > 0.99, "p={} should be > 0.99", p);
    }

    #[test]
    fn test_p_yes_far_below_strike() {
        // Forecast high = 40°F, strike = 70°F → very unlikely YES
        let fc = make_forecast(40.0, 30.0, 3.0);
        let p = compute_p_yes(&fc, "greater", Some(70), None);
        assert!(p < 0.01, "p={} should be < 0.01", p);
    }

    #[test]
    fn test_p_yes_at_strike() {
        // Forecast high = 50°F, strike = 50°F → ~50%
        let fc = make_forecast(50.0, 40.0, 3.0);
        let p = compute_p_yes(&fc, "greater", Some(50), None);
        assert!((p - 0.5).abs() < 0.05, "p={} should be ~0.5", p);
    }

    #[test]
    fn test_p_yes_between() {
        // Forecast high = 50°F, between 45-55 with std_dev=3 → high probability
        let fc = make_forecast(50.0, 40.0, 3.0);
        let p = compute_p_yes(&fc, "between", Some(45), Some(55));
        assert!(p > 0.8, "p={} should be > 0.8", p);
    }

    #[test]
    fn test_p_yes_less() {
        // Forecast low = 30°F, strike = 40°F → very likely YES
        let fc = make_forecast(50.0, 30.0, 3.0);
        let p = compute_p_yes(&fc, "less", None, Some(40));
        assert!(p > 0.95, "p={} should be > 0.95", p);
    }

    #[test]
    fn test_normal_cdf_symmetry() {
        let cdf_pos = normal_cdf(1.0);
        let cdf_neg = normal_cdf(-1.0);
        assert!(
            (cdf_pos + cdf_neg - 1.0).abs() < 0.02,
            "CDF should be approximately symmetric"
        );
    }
}
