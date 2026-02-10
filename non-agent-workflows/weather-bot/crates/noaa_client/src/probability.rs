//! Forecast-to-probability conversion.
//!
//! Converts NOAA temperature forecasts into probabilities for
//! Kalshi weather market settlement.
//!
//! Uses Abramowitz & Stegun rational approximation for the normal CDF
//! (accuracy <1e-7), returns confidence intervals, and adjusts for
//! precipitation effects on temperature.

use common::ForecastData;

// ── Public Types ──────────────────────────────────────────────────────

/// A probability estimate with confidence bounds.
#[derive(Debug, Clone, Copy)]
pub struct ProbabilityEstimate {
    /// Point estimate of P(YES).
    pub p_yes: f64,
    /// Forecast confidence (0.0–1.0): how tight the uncertainty is.
    /// High confidence (>0.8) means std_dev is small relative to distance-to-strike.
    pub confidence: f64,
    /// Conservative (5th percentile) bound on p_yes.
    pub p_yes_low: f64,
    /// Optimistic (95th percentile) bound on p_yes.
    pub p_yes_high: f64,
}

// ── Main API ──────────────────────────────────────────────────────────

/// Compute a full probability estimate for a weather market.
///
/// # Arguments
/// * `forecast` — NOAA forecast data for the relevant city
/// * `strike_type` — `"greater"`, `"less"`, or `"between"`
/// * `floor_strike` — lower strike bound (used by "greater" and "between")
/// * `cap_strike` — upper strike bound (used by "less" and "between")
pub fn compute_probability(
    forecast: &ForecastData,
    strike_type: &str,
    floor_strike: Option<f64>,
    cap_strike: Option<f64>,
) -> ProbabilityEstimate {
    let std_dev = adjusted_std_dev(forecast);

    // Compute the point estimate.
    let p_yes = compute_p_yes_inner(forecast, strike_type, floor_strike, cap_strike, std_dev);

    // Compute confidence bounds by perturbing the std_dev.
    // Higher uncertainty (wider std_dev) → wider bounds.
    let std_dev_high = std_dev * 1.5; // pessimistic uncertainty
    let std_dev_low = (std_dev * 0.7).max(1.5); // optimistic uncertainty

    let p_variant_a = compute_p_yes_inner(
        forecast,
        strike_type,
        floor_strike,
        cap_strike,
        std_dev_high,
    );
    let p_variant_b =
        compute_p_yes_inner(forecast, strike_type, floor_strike, cap_strike, std_dev_low);

    let p_yes_low = p_variant_a.min(p_variant_b);
    let p_yes_high = p_variant_a.max(p_variant_b);

    // Confidence metric: combines two factors:
    // 1. How far p_yes is from 0.5 (distance-to-strike signal strength).
    //    At p=0.5, the outcome is maximally uncertain → low confidence.
    //    At p=0.95 or p=0.05, the signal is strong → high confidence.
    // 2. The spread between optimistic/pessimistic bounds.
    let distance_from_uncertain = (2.0 * (p_yes - 0.5)).abs(); // 0.0 at p=0.5, 1.0 at p=0/1
    let spread = (p_yes_high - p_yes_low).max(0.0);
    let spread_penalty = (1.0 - 4.0 * spread).clamp(0.0, 1.0);
    let confidence = (distance_from_uncertain * spread_penalty).clamp(0.0, 1.0);

    ProbabilityEstimate {
        p_yes,
        confidence,
        p_yes_low,
        p_yes_high,
    }
}

/// Backward-compatible wrapper — returns just p_yes.
pub fn compute_p_yes(
    forecast: &ForecastData,
    strike_type: &str,
    floor_strike: Option<f64>,
    cap_strike: Option<f64>,
) -> f64 {
    compute_probability(forecast, strike_type, floor_strike, cap_strike).p_yes
}

// ── Internal Helpers ──────────────────────────────────────────────────

/// Adjust std_dev for precipitation effects.
///
/// Heavy precipitation suppresses temperature highs and raises lows
/// by increasing cloud cover and evaporative cooling.
fn adjusted_std_dev(forecast: &ForecastData) -> f64 {
    let base_std = forecast.temp_std_dev.max(2.0);

    // High precipitation probability increases uncertainty and shifts distribution.
    // At 80%+ precip, add ~30% to uncertainty.
    let precip_factor = 1.0 + 0.4 * forecast.precip_prob;

    base_std * precip_factor
}

/// Temperature adjustment for precipitation.
/// Heavy precip suppresses highs by ~2-4°F and raises lows by ~1-2°F.
fn precip_adjusted_high(forecast: &ForecastData) -> f64 {
    forecast.high_temp_f - 4.0 * forecast.precip_prob.powi(2)
}

fn precip_adjusted_low(forecast: &ForecastData) -> f64 {
    forecast.low_temp_f + 2.0 * forecast.precip_prob.powi(2)
}

/// Core p_yes computation for a given std_dev.
fn compute_p_yes_inner(
    forecast: &ForecastData,
    strike_type: &str,
    floor_strike: Option<f64>,
    cap_strike: Option<f64>,
    std_dev: f64,
) -> f64 {
    match strike_type {
        "greater" | "greater_or_equal" => {
            // P(high_temp >= strike)
            let strike = floor_strike.unwrap_or(0.0);
            let mean = precip_adjusted_high(forecast);
            let z = (mean - strike) / std_dev;
            normal_cdf(z)
        }
        "less" | "less_or_equal" => {
            // P(low_temp <= strike)
            let strike = cap_strike.unwrap_or(100.0);
            let mean = precip_adjusted_low(forecast);
            let z = (strike - mean) / std_dev;
            normal_cdf(z)
        }
        "between" => {
            // P(floor <= temp <= cap)
            // Use midpoint of high/low as the relevant mean.
            let floor = floor_strike.unwrap_or(0.0);
            let cap = cap_strike.unwrap_or(100.0);
            let high = precip_adjusted_high(forecast);
            let low = precip_adjusted_low(forecast);
            let mean = (high + low) / 2.0;

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

// ── Normal CDF (Abramowitz & Stegun 26.2.17) ─────────────────────────

/// Accurate normal CDF approximation.
///
/// Uses the rational approximation from Abramowitz & Stegun (1964),
/// formula 26.2.17. Maximum error < 7.5e-8 across all z.
///
/// For comparison, the old logistic approximation had ~1% error.
fn normal_cdf(z: f64) -> f64 {
    if z < -8.0 {
        return 0.0;
    }
    if z > 8.0 {
        return 1.0;
    }

    if z < 0.0 {
        return 1.0 - normal_cdf(-z);
    }

    // Constants from A&S 26.2.17
    const B0: f64 = 0.2316419;
    const B1: f64 = 0.319381530;
    const B2: f64 = -0.356563782;
    const B3: f64 = 1.781477937;
    const B4: f64 = -1.821255978;
    const B5: f64 = 1.330274429;

    let t = 1.0 / (1.0 + B0 * z);
    let t2 = t * t;
    let t3 = t2 * t;
    let t4 = t3 * t;
    let t5 = t4 * t;

    let poly = B1 * t + B2 * t2 + B3 * t3 + B4 * t4 + B5 * t5;
    let pdf = (-0.5 * z * z).exp() / (2.0 * std::f64::consts::PI).sqrt();

    1.0 - pdf * poly
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

    fn make_forecast_with_precip(high: f64, low: f64, std_dev: f64, precip: f64) -> ForecastData {
        ForecastData {
            city: "Test".into(),
            high_temp_f: high,
            low_temp_f: low,
            precip_prob: precip,
            temp_std_dev: std_dev,
            fetched_at: Utc::now(),
        }
    }

    // ── CDF accuracy tests ────────────────────────────────────────────

    #[test]
    fn test_rational_cdf_at_zero() {
        let cdf = normal_cdf(0.0);
        assert!((cdf - 0.5).abs() < 1e-7, "CDF(0) = {} should be 0.5", cdf);
    }

    #[test]
    fn test_rational_cdf_symmetry() {
        for z in [0.5, 1.0, 1.5, 2.0, 3.0] {
            let sum = normal_cdf(z) + normal_cdf(-z);
            assert!(
                (sum - 1.0).abs() < 1e-7,
                "CDF({}) + CDF(-{}) = {} should be 1.0",
                z,
                z,
                sum
            );
        }
    }

    #[test]
    fn test_rational_cdf_known_values() {
        // Reference values from standard normal table.
        let cases = [
            (1.0, 0.8413447),
            (2.0, 0.9772499),
            (3.0, 0.9986501),
            (-1.0, 0.1586553),
            (-2.0, 0.0227501),
        ];
        for (z, expected) in cases {
            let got = normal_cdf(z);
            assert!(
                (got - expected).abs() < 1e-5,
                "CDF({}) = {}, expected ~{}",
                z,
                got,
                expected
            );
        }
    }

    // ── Strike-type tests ──────────────────────────────────────────────

    #[test]
    fn test_p_yes_far_above_strike() {
        // Forecast high = 80°F, strike = 50°F, std_dev = 3°F → very likely YES
        let fc = make_forecast(80.0, 60.0, 3.0);
        let p = compute_p_yes(&fc, "greater", Some(50.0), None);
        assert!(p > 0.99, "p={} should be > 0.99", p);
    }

    #[test]
    fn test_p_yes_far_below_strike() {
        // Forecast high = 40°F, strike = 70°F → very unlikely YES
        let fc = make_forecast(40.0, 30.0, 3.0);
        let p = compute_p_yes(&fc, "greater", Some(70.0), None);
        assert!(p < 0.01, "p={} should be < 0.01", p);
    }

    #[test]
    fn test_p_yes_at_strike() {
        // Forecast high = 50°F, strike = 50°F → ~50%
        let fc = make_forecast(50.0, 40.0, 3.0);
        let p = compute_p_yes(&fc, "greater", Some(50.0), None);
        assert!((p - 0.5).abs() < 0.05, "p={} should be ~0.5", p);
    }

    #[test]
    fn test_p_yes_between() {
        // Forecast high=50, low=40 → mean=45; between 40-50 with std_dev=3 → high P
        let fc = make_forecast(50.0, 40.0, 3.0);
        let p = compute_p_yes(&fc, "between", Some(40.0), Some(50.0));
        assert!(p > 0.8, "p={} should be > 0.8", p);
    }

    #[test]
    fn test_p_yes_less() {
        // Forecast low = 30°F, strike = 40°F → very likely YES
        let fc = make_forecast(50.0, 30.0, 3.0);
        let p = compute_p_yes(&fc, "less", None, Some(40.0));
        assert!(p > 0.95, "p={} should be > 0.95", p);
    }

    // ── Precipitation adjustment tests ─────────────────────────────────

    #[test]
    fn test_precip_suppresses_high_temp_probability() {
        let fc_dry = make_forecast_with_precip(55.0, 40.0, 3.0, 0.0);
        let fc_wet = make_forecast_with_precip(55.0, 40.0, 3.0, 0.9);

        let p_dry = compute_p_yes(&fc_dry, "greater", Some(53.0), None);
        let p_wet = compute_p_yes(&fc_wet, "greater", Some(53.0), None);

        assert!(
            p_wet < p_dry,
            "Wet p={} should be lower than dry p={} for high-temp markets",
            p_wet,
            p_dry
        );
    }

    #[test]
    fn test_precip_raises_low_temp_probability() {
        // Heavy precip raises lows → P(low < strike) may decrease
        let fc_dry = make_forecast_with_precip(60.0, 35.0, 3.0, 0.0);
        let fc_wet = make_forecast_with_precip(60.0, 35.0, 3.0, 0.9);

        let p_dry = compute_p_yes(&fc_dry, "less", None, Some(34.0));
        let p_wet = compute_p_yes(&fc_wet, "less", None, Some(34.0));

        // Wet conditions raise the effective low, so P(low <= 34) should decrease
        assert!(
            p_wet < p_dry,
            "Wet p={} should be lower than dry p={} for very-low-strike",
            p_wet,
            p_dry
        );
    }

    // ── Confidence bound tests ─────────────────────────────────────────

    #[test]
    fn test_confidence_bounds_ordering() {
        let fc = make_forecast(60.0, 45.0, 4.0);
        let est = compute_probability(&fc, "greater", Some(55.0), None);

        assert!(
            est.p_yes_low <= est.p_yes,
            "p_yes_low={} should be <= p_yes={}",
            est.p_yes_low,
            est.p_yes
        );
        assert!(
            est.p_yes <= est.p_yes_high,
            "p_yes={} should be <= p_yes_high={}",
            est.p_yes,
            est.p_yes_high
        );
    }

    #[test]
    fn test_high_confidence_for_clear_signal() {
        // Very clear signal: high=80, strike=50, std_dev=2 → high confidence
        let fc = make_forecast(80.0, 60.0, 2.0);
        let est = compute_probability(&fc, "greater", Some(50.0), None);

        assert!(
            est.confidence > 0.9,
            "confidence={} should be > 0.9 for a very clear signal",
            est.confidence
        );
    }

    #[test]
    fn test_low_confidence_near_strike() {
        // Strike exactly at mean with very high uncertainty
        let fc = make_forecast(50.0, 40.0, 12.0);
        let est = compute_probability(&fc, "greater", Some(50.0), None);

        assert!(
            est.confidence < 0.9,
            "confidence={} should be < 0.9 for uncertain near-strike",
            est.confidence
        );
    }
}
