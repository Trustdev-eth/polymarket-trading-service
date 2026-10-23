//! Public Polymarket data-api reads. No private key.

use reqwest::Client;
use serde_json::Value;

const VALUE_URL: &str = "https://data-api.polymarket.com/value";

pub async fn funder_value(http: &Client, funder: &str) -> Result<f64, String> {
    let user = funder.trim();
    if user.len() < 8 {
        return Err("Missing funder address.".into());
    }
    let url = format!("{VALUE_URL}?user={user}");
    let res = http
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("data-api: {e}"))?;
    if !res.status().is_success() {
        return Err(format!("data-api HTTP {}", res.status()));
    }
    let v: Value = res.json().await.map_err(|e| format!("data-api body: {e}"))?;
    parse_value(&v).ok_or_else(|| "data-api returned no value".into())
}

pub fn parse_value(v: &Value) -> Option<f64> {
    fn num(x: &Value) -> Option<f64> {
        x.as_f64()
            .or_else(|| x.as_i64().map(|n| n as f64))
            .or_else(|| x.as_u64().map(|n| n as f64))
            .or_else(|| x.as_str().and_then(|s| s.trim().parse().ok()))
    }
    if let Some(n) = num(v) {
        return Some(n);
    }
    if let Some(n) = v.get("value").and_then(num) {
        return Some(n);
    }
    if let Some(arr) = v.as_array() {
        let mut sum = 0.0;
        let mut any = false;
        for item in arr {
            if let Some(n) = item.get("value").and_then(num).or_else(|| num(item)) {
                sum += n;
                any = true;
            }
        }
        if any {
            return Some(sum);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_object_string() {
        assert_eq!(parse_value(&json!({"value": "200.5"})), Some(200.5));
    }

    #[test]
    fn parses_array_sum() {
        assert_eq!(
            parse_value(&json!([{"user": "0x1", "value": 100}, {"value": "50"}])),
            Some(150.0)
        );
    }
}
