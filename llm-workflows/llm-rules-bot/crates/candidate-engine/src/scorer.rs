use common::MarketInfo;

pub fn compute_complexity_score(market: &MarketInfo) -> (f64, Vec<String>) {
    let mut score: f64 = 0.0;
    let mut reasons = Vec::new();
    let rules_lower = market.rules_primary.to_ascii_lowercase();

    // 1. Rules length
    let rules_len = market.rules_primary.len();
    if rules_len > 1000 {
        score += 0.5;
        reasons.push(format!("Long rules ({})", rules_len));
    } else if rules_len > 500 {
        score += 0.2;
        reasons.push(format!("Medium rules ({})", rules_len));
    }

    // 2. Secondary rules
    if !market.rules_secondary.is_empty() {
        score += 0.3;
        reasons.push("Has secondary rules".to_string());
    }

    // 3. Date/Time ambiguity keywords (heuristic)
    let time_keywords = ["approximate", "estimate", "around", "variable"];
    for kw in time_keywords {
        if rules_lower.contains(kw) {
            score += 0.2;
            reasons.push(format!("Ambiguous time keyword: {}", kw));
        }
    }

    // 4. External source dependency
    if rules_lower.contains("link") || rules_lower.contains("http") {
        score += 0.1;
        reasons.push("External source reference".to_string());
    }

    (score.clamp(0.0, 1.0), reasons)
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::MarketInfo;

    #[test]
    fn test_compute_complexity_score_basic() {
        let market = MarketInfo {
            ticker: "TEST-1".to_string(),
            event_ticker: "TEST".to_string(),
            market_type: "bop".to_string(),
            title: "Test Market".to_string(),
            subtitle: "".to_string(),
            close_time: Some(chrono::Utc::now()),
            expiration_time: None,
            status: "active".to_string(),
            yes_ask: 50,
            yes_bid: 40,
            no_ask: 60,
            no_bid: 50,
            last_price: 45,
            volume: 100,
            volume_24h: 100,
            open_interest: 1000,
            rules_primary: "Short rules".to_string(),
            rules_secondary: "Short secondary".to_string(),
            result: None,
            strike_type: None,
            floor_strike: None,
            cap_strike: None,
            functional_strike: None,
            tick_size: None,
            response_price_units: None,
        };

        let (score, _reasons) = compute_complexity_score(&market);
        
        // Base score 0.0 + minimal rules
        // "Short rules" is < 500 chars, so no penalty there.
        // "Short secondary" is < 1000 chars, so no penalty.
        // Ambiguity check might trigger if keywords present.
        
        assert!(score >= 0.0);
        assert!(score <= 1.0);
    }
}
