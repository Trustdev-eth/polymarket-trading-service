use hmac::{Hmac, Mac};
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use sha2::Sha512;

use crate::error::{AppError, AppResult};

const API: &str = "https://api.nowpayments.io/v1";
const UNLOCK: &[&str] = &["finished", "confirmed", "sending"];

pub fn sort_keys(v: &Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<_> = map.keys().cloned().collect();
            keys.sort();
            let mut out = Map::new();
            for k in keys {
                out.insert(k.clone(), sort_keys(&map[&k]));
            }
            Value::Object(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(sort_keys).collect()),
        other => other.clone(),
    }
}

pub fn verify_ipn(raw: &str, signature: Option<&str>, secret: &str) -> bool {
    let Some(sig) = signature.filter(|s| !s.is_empty()) else {
        return false;
    };
    if secret.is_empty() {
        return false;
    }
    let Ok(payload) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    let sorted = serde_json::to_string(&sort_keys(&payload)).unwrap_or_default();
    let mut mac = match Hmac::<Sha512>::new_from_slice(secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(sorted.as_bytes());
    let digest = hex::encode(mac.finalize().into_bytes());
    if digest.len() != sig.len() {
        return false;
    }
    digest
        .as_bytes()
        .iter()
        .zip(sig.as_bytes())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

pub fn unlocks(status: &str) -> bool {
    UNLOCK.contains(&status.to_lowercase().as_str())
}

pub struct Invoice {
    pub id: String,
    pub url: String,
}

pub async fn create_invoice(
    http: &Client,
    api_key: &str,
    price_usd: f64,
    order_id: &str,
    description: &str,
    success_url: &str,
    cancel_url: &str,
    ipn_url: &str,
) -> AppResult<Invoice> {
    if api_key.is_empty() {
        return Err(AppError::Pay("NOWPAYMENTS_API_KEY is missing".into()));
    }
    let res = http
        .post(format!("{API}/invoice"))
        .header("x-api-key", api_key)
        .json(&json!({
            "price_amount": price_usd,
            "price_currency": "usd",
            "order_id": order_id,
            "order_description": description,
            "ipn_callback_url": ipn_url,
            "success_url": success_url,
            "cancel_url": cancel_url,
        }))
        .send()
        .await
        .map_err(|e| AppError::Pay(e.to_string()))?;
    let status = res.status();
    let body: Value = res.json().await.map_err(|e| AppError::Pay(e.to_string()))?;
    let url = body
        .get("invoice_url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if !status.is_success() || url.is_empty() {
        let msg = body
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("NOWPayments invoice failed");
        return Err(AppError::Pay(msg.into()));
    }
    Ok(Invoice {
        id: body
            .get("id")
            .map(|v| v.to_string().trim_matches('"').to_string())
            .unwrap_or_default(),
        url,
    })
}

#[derive(Deserialize)]
struct PaymentRow {
    #[serde(default)]
    order_id: String,
    #[serde(default)]
    invoice_id: Value,
    #[serde(default)]
    payment_status: String,
}

pub async fn payment_status(
    http: &Client,
    api_key: &str,
    order_id: &str,
    invoice_id: &str,
    payment_id: &str,
) -> AppResult<Option<String>> {
    if !payment_id.is_empty() {
        let res = http
            .get(format!("{API}/payment/{payment_id}"))
            .header("x-api-key", api_key)
            .send()
            .await
            .map_err(|e| AppError::Pay(e.to_string()))?;
        if res.status().is_success() {
            let body: Value = res.json().await.unwrap_or(Value::Null);
            let oid = body.get("order_id").and_then(|v| v.as_str()).unwrap_or("");
            if oid.is_empty() || oid == order_id {
                if let Some(s) = body.get("payment_status").and_then(|v| v.as_str()) {
                    return Ok(Some(s.to_lowercase()));
                }
            }
        }
    }
    if invoice_id.is_empty() {
        return Ok(None);
    }
    let url = format!("{API}/payment/?invoiceid={invoice_id}&limit=50");
    let res = http
        .get(url)
        .header("x-api-key", api_key)
        .send()
        .await
        .map_err(|e| AppError::Pay(e.to_string()))?;
    if !res.status().is_success() {
        return Ok(None);
    }
    let body: Value = res.json().await.unwrap_or(Value::Null);
    let rows: Vec<PaymentRow> = if let Some(arr) = body.as_array() {
        serde_json::from_value(Value::Array(arr.clone())).unwrap_or_default()
    } else if let Some(arr) = body.get("data").and_then(|v| v.as_array()) {
        serde_json::from_value(Value::Array(arr.clone())).unwrap_or_default()
    } else {
        Vec::new()
    };
    let mut first = None;
    for row in rows {
        let inv = row.invoice_id.to_string().trim_matches('"').to_string();
        if row.order_id != order_id && inv != invoice_id {
            continue;
        }
        let st = row.payment_status.to_lowercase();
        if first.is_none() {
            first = Some(st.clone());
        }
        if unlocks(&st) {
            return Ok(Some(st));
        }
    }
    Ok(first)
}

pub fn ipn_callback_url(app_url: &str) -> String {
    let base = app_url.trim_end_matches('/');
    if base.contains("localhost") || base.contains("127.0.0.1") {
        "https://trade.ohioismquant.vip/api/pay/ipn".into()
    } else {
        format!("{base}/api/pay/ipn")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hmac::{Hmac, Mac};
    use sha2::Sha512;

    #[test]
    fn sorts_object_keys() {
        let v: Value = serde_json::from_str(r#"{"b":1,"a":{"z":1,"m":2}}"#).unwrap();
        assert_eq!(
            serde_json::to_string(&sort_keys(&v)).unwrap(),
            r#"{"a":{"m":2,"z":1},"b":1}"#
        );
    }

    #[test]
    fn ipn_hmac_accepts_sorted_body() {
        let raw = r#"{"payment_status":"finished","order_id":"t_1"}"#;
        let secret = "ipn-secret";
        let payload: Value = serde_json::from_str(raw).unwrap();
        let sorted = serde_json::to_string(&sort_keys(&payload)).unwrap();
        let mut mac = Hmac::<Sha512>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(sorted.as_bytes());
        let sig = hex::encode(mac.finalize().into_bytes());
        assert!(verify_ipn(raw, Some(&sig), secret));
        assert!(!verify_ipn(raw, Some("deadbeef"), secret));
        assert!(!verify_ipn(raw, None, secret));
    }

    #[test]
    fn unlock_statuses() {
        assert!(unlocks("finished"));
        assert!(unlocks("CONFIRMED"));
        assert!(!unlocks("waiting"));
    }
}
