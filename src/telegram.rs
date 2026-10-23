use reqwest::Client;
use serde_json::json;

/// Best-effort admin ping. Never fails the request path.
pub async fn notify(http: &Client, token: &str, chat_id: &str, text: &str) {
    if token.is_empty() || chat_id.is_empty() {
        return;
    }
    let url = format!("https://api.telegram.org/bot{token}/sendMessage");
    let _ = http
        .post(url)
        .json(&json!({ "chat_id": chat_id, "text": text }))
        .send()
        .await;
}
