use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::amounts::order_amounts;
use crate::auth::{content_headers, l2_headers, ApiCreds};
use crate::eip712::{hex0x, u256_to_dec, ZERO_BYTES32};
use crate::sign::{OrderV2, V2Signer};

pub const CLOB_HOST: &str = "https://clob.polymarket.com";

#[derive(Clone, Debug)]
pub struct MarketResolved {
    pub yes_id: [u8; 32],
    pub no_id: [u8; 32],
    pub tick_size: f64,
    pub fee_bps: u32,
    pub neg_risk: bool,
}

#[derive(Clone)]
pub struct ClobClient {
    pub host: String,
    pub signer: V2Signer,
    pub creds: ApiCreds,
    pub funder: [u8; 20],
    pub sig_type: u8,
    pub paper: bool,
    /// Address bound to the L2 API key. Official SDK uses the EOA; override with POLY_ADDRESS.
    pub l2_address: [u8; 20],
    #[cfg_attr(windows, allow(dead_code))]
    http: reqwest::Client,
    /// Pre-resolved — never GET on the order path.
    pub market: Option<MarketResolved>,
}

impl ClobClient {
    pub fn new(
        host: String,
        signer: V2Signer,
        creds: ApiCreds,
        funder: [u8; 20],
        sig_type: u8,
        paper: bool,
    ) -> Self {
        let l2_address = signer.address();
        Self::new_with_l2(host, signer, creds, funder, sig_type, paper, l2_address)
    }

    pub fn new_with_l2(
        host: String,
        signer: V2Signer,
        creds: ApiCreds,
        funder: [u8; 20],
        sig_type: u8,
        paper: bool,
        l2_address: [u8; 20],
    ) -> Self {
        Self {
            host,
            signer,
            creds,
            funder,
            sig_type,
            paper,
            l2_address,
            http: reqwest::Client::builder()
                .no_proxy()
                .tcp_nodelay(true)
                .tcp_keepalive(Duration::from_secs(15))
                .pool_max_idle_per_host(8)
                .pool_idle_timeout(Duration::from_secs(90))
                .connect_timeout(Duration::from_millis(800))
                .timeout(Duration::from_secs(4))
                .http2_adaptive_window(true)
                .build()
                .expect("reqwest"),
            market: None,
        }
    }

    pub fn set_market(&mut self, m: MarketResolved) {
        self.market = Some(m);
    }

    /// Warm the TLS + HTTP/2 session so the first live POST is not a handshake.
    pub async fn warmup(&self) {
        let url = format!("{}/time", self.host);
        #[cfg(windows)]
        {
            let url = url.clone();
            let _ = tokio::task::spawn_blocking(move || {
                let tls = native_tls::TlsConnector::new().ok()?;
                let agent = ureq::AgentBuilder::new()
                    .tls_connector(std::sync::Arc::new(tls))
                    .timeout(Duration::from_secs(8))
                    .build();
                agent.get(&url).call().ok()
            })
            .await;
        }
        #[cfg(not(windows))]
        {
            let _ = self.http.get(url).send().await;
        }
    }

    fn order_signer_addr(&self) -> [u8; 20] {
        if self.sig_type == crate::eip712::SIG_TYPE_POLY_1271 {
            self.funder
        } else {
            self.signer.address()
        }
    }

    /// Sign a limit. Returns (body json bytes, digest, cloid hex).
    pub fn sign_limit(
        &self,
        token_id: [u8; 32],
        is_buy: bool,
        price: f64,
        size: f64,
        tif: &str,
        post_only: bool,
        timestamp_ms: u64,
        salt: u128,
        expiration: u64,
    ) -> Result<(Vec<u8>, [u8; 32], String), String> {
        let tick = self.market.as_ref().map(|m| m.tick_size).unwrap_or(0.01);
        let neg_risk = self.market.as_ref().map(|m| m.neg_risk).unwrap_or(false);
        let amt = order_amounts(is_buy, size, price, tick);
        let order_signer = self.order_signer_addr();
        let order = OrderV2 {
            salt,
            maker: self.funder,
            signer: order_signer,
            token_id,
            maker_amount: amt.maker,
            taker_amount: amt.taker,
            side: amt.side,
            signature_type: self.sig_type,
            timestamp: timestamp_ms,
            metadata: ZERO_BYTES32,
            builder: ZERO_BYTES32,
        };
        let (sig, cloid, digest) = self
            .signer
            .sign_order(&order, neg_risk)
            .map_err(|e| e.to_string())?;
        // Field order and types match py-clob-client-v2 `order_to_json_v2` (HMAC body).
        let salt_num = serde_json::Number::from(salt as u64);
        let body = json!({
            "order": {
                "salt": salt_num,
                "maker": crate::eip712::hex0x(&self.funder),
                "signer": crate::eip712::hex0x(&order_signer),
                "tokenId": u256_to_dec(&token_id),
                "makerAmount": amt.maker.to_string(),
                "takerAmount": amt.taker.to_string(),
                "side": if is_buy { "BUY" } else { "SELL" },
                "expiration": expiration.to_string(),
                "signatureType": self.sig_type,
                "timestamp": timestamp_ms.to_string(),
                "metadata": hex0x(&ZERO_BYTES32),
                "builder": hex0x(&ZERO_BYTES32),
                "signature": sig,
            },
            "owner": self.creds.api_key,
            "orderType": tif,
            "deferExec": false,
            "postOnly": post_only,
        });
        let bytes = serde_json::to_vec(&body).map_err(|e| e.to_string())?;
        Ok((bytes, digest, cloid))
    }

    pub async fn post_orders_bytes(&self, body: &[u8]) -> Result<Value, String> {
        self.post_order_bytes(body).await
    }

    /// Single order: POST /order (not /orders). Compact JSON already in `body`.
    pub async fn post_order_bytes(&self, body: &[u8]) -> Result<Value, String> {
        if self.paper {
            return Ok(json!({"paper": true, "posted": false}));
        }
        self.l2_json("POST", "/order", Some(body)).await
    }

    pub async fn cancel_bytes(&self, body: &[u8], batch: bool) -> Result<Value, String> {
        if self.paper {
            return Ok(json!({"paper": true, "canceled": true}));
        }
        let path = if batch { "/orders" } else { "/order" };
        self.l2_json("DELETE", path, Some(body)).await
    }

    pub async fn cancel_order_id(&self, cloid: &str) -> Result<Value, String> {
        let body = serde_json::to_vec(&json!({"orderID": cloid})).unwrap();
        self.cancel_bytes(&body, false).await
    }

    pub async fn cancel_all(&self) -> Result<Value, String> {
        if self.paper {
            return Ok(json!({"paper": true, "canceled": true}));
        }
        self.l2_json("DELETE", "/cancel-all", None).await
    }

    /// Cancel resting orders for one CLOB token (YES or NO).
    pub async fn cancel_market_orders(&self, asset_id: &str) -> Result<Value, String> {
        if self.paper {
            return Ok(json!({"paper": true, "canceled": true}));
        }
        let body = serde_json::to_vec(&json!({ "asset_id": asset_id })).unwrap();
        self.l2_json("DELETE", "/cancel-market-orders", Some(&body)).await
    }

    /// Recent user trades used to seed inventory after Hello.
    pub async fn fetch_trades(&self) -> Result<Value, String> {
        if self.paper {
            return Ok(json!([]));
        }
        self.l2_json("GET", "/data/trades", None).await
    }

    async fn l2_json(&self, method: &str, path: &str, body: Option<&[u8]>) -> Result<Value, String> {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let body_str = body.map(|b| String::from_utf8_lossy(b).into_owned());
        let mut headers = content_headers();
        headers.push(("Connection".into(), "keep-alive".into()));
        headers.extend(l2_headers(
            crate::eip712::hex0x(&self.l2_address),
            &self.creds,
            ts,
            method,
            path,
            body_str.as_deref(),
        ));
        let url = format!("{}{path}", self.host);
        #[cfg(windows)]
        {
            return win_l2_json(method, url, headers, body).await;
        }
        #[cfg(not(windows))]
        {
        let mut req = match method {
            "POST" => self.http.post(&url),
            "DELETE" => self.http.delete(&url),
            "GET" => self.http.get(&url),
            _ => return Err("method".into()),
        };
        for (k, v) in headers {
            req = req.header(k, v);
        }
        if let Some(b) = body {
            req = req.body(b.to_vec());
        }
        let started = Instant::now();
        let resp = req.send().await.map_err(|e| e.to_string())?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| e.to_string())?;
        tracing::info!(
            method,
            path,
            status = status.as_u16(),
            rtt_ms = started.elapsed().as_millis() as u64,
            "clob http"
        );
        if !status.is_success() {
            return Err(format!("{status}: {text}"));
        }
        if text.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).map_err(|e| e.to_string())
        }
    }
}

#[cfg(windows)]
async fn win_l2_json(
    method: &str,
    url: String,
    headers: Vec<(String, String)>,
    body: Option<&[u8]>,
) -> Result<Value, String> {
    let method = method.to_string();
    let body = body.map(|b| b.to_vec());
    tokio::task::spawn_blocking(move || {
        let started = Instant::now();
        let mut req = win_ureq_agent().map_err(|e| e.to_string())?.request(&method, &url);
        for (k, v) in &headers {
            req = req.set(k, v);
        }
        let resp = match body {
            Some(b) => req.send_bytes(&b),
            None => req.call(),
        }
        .map_err(|e| e.to_string())?;
        let status = resp.status();
        let text = resp.into_string().map_err(|e| e.to_string())?;
        tracing::info!(
            method = method.as_str(),
            url = url.as_str(),
            status,
            rtt_ms = started.elapsed().as_millis() as u64,
            "clob http"
        );
        if !(200..300).contains(&status) {
            return Err(format!("{status}: {text}"));
        }
        if text.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(windows)]
fn win_ureq_agent() -> Result<ureq::Agent, String> {
    let tls = native_tls::TlsConnector::new().map_err(|e| e.to_string())?;
    Ok(ureq::AgentBuilder::new()
        .tls_connector(std::sync::Arc::new(tls))
        .timeout(Duration::from_secs(8))
        .build())
}

async fn json_get(url: &str) -> Result<Value, String> {
    let url = url.to_string();
    #[cfg(windows)]
    {
        tokio::task::spawn_blocking(move || {
            let resp = win_ureq_agent()
                .map_err(|e| e.to_string())?
                .get(&url)
                .call()
                .map_err(|e| e.to_string())?;
            resp.into_json().map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }
    #[cfg(not(windows))]
    {
        let http = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(8))
            .build()
            .map_err(|e| e.to_string())?;
        http.get(url)
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())
    }
}

/// Startup-only market metadata. Forbidden on the order hot path.
pub async fn fetch_tick_fee_negrisk(
    host: &str,
    token_id_dec: &str,
) -> Result<(f64, u32, bool), String> {
    let tick: Value = json_get(&format!("{host}/tick-size?token_id={token_id_dec}")).await?;
    let fee: Value = json_get(&format!("{host}/fee-rate?token_id={token_id_dec}")).await?;
    let nr: Value = json_get(&format!("{host}/neg-risk?token_id={token_id_dec}")).await?;
    let tick_size = tick
        .get("minimum_tick_size")
        .or_else(|| tick.get("tick_size"))
        .and_then(|v| v.as_str().and_then(|s| s.parse().ok()).or_else(|| v.as_f64()))
        .unwrap_or(0.01);
    let fee_bps = fee
        .get("base_fee")
        .or_else(|| fee.get("fee_rate_bps"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let neg_risk = nr
        .get("neg_risk")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    Ok((tick_size, fee_bps, neg_risk))
}

/// Matching holds for a market: (`itode` crypto taker delay on, sports `seconds_delay`).
pub async fn fetch_match_delay(host: &str, condition_id: &str) -> Result<(bool, u64), String> {
    let clob_url = format!("{host}/clob-markets/{condition_id}");
    let market_url = format!("{host}/markets/{condition_id}");
    let (clob, market) = tokio::join!(json_get(&clob_url), json_get(&market_url));
    let itode = clob?.get("itode").and_then(|v| v.as_bool()).unwrap_or(false);
    let seconds = market?
        .get("seconds_delay")
        .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
        .unwrap_or(0);
    Ok((itode, seconds))
}

/// 500ms ack cache for fast-cancel (lecture 40). cloid is known at sign time.
pub struct FastCancelCache {
    /// client_seq → (cloid hex, signed_at)
    pending: HashMap<u64, (String, Instant)>,
    /// cancels that arrived before POST returned
    queued: HashMap<u64, Instant>,
}

impl FastCancelCache {
    pub fn new() -> Self {
        Self {
            pending: HashMap::with_capacity(64),
            queued: HashMap::with_capacity(64),
        }
    }

    pub fn on_signed(&mut self, seq: u64, cloid: String) {
        self.pending.insert(seq, (cloid, Instant::now()));
    }

    pub fn seq_for_cloid(&self, cloid: &str) -> Option<u64> {
        let want = cloid.trim().trim_start_matches("0x").to_ascii_lowercase();
        self.pending.iter().find_map(|(seq, (c, _))| {
            let have = c.trim().trim_start_matches("0x").to_ascii_lowercase();
            if have == want || have.ends_with(&want) || want.ends_with(&have) {
                Some(*seq)
            } else {
                None
            }
        })
    }

    pub fn on_post_done(&mut self, seq: u64) -> Option<String> {
        let cloid = self.pending.get(&seq).map(|(c, _)| c.clone());
        if self.queued.remove(&seq).is_some() {
            cloid
        } else {
            None
        }
    }

    /// Returns cloid immediately if we already signed. Otherwise queue for ≤500ms.
    pub fn on_cancel(&mut self, seq: u64) -> Option<String> {
        if let Some((cloid, _)) = self.pending.get(&seq) {
            return Some(cloid.clone());
        }
        self.queued.insert(seq, Instant::now());
        None
    }

    pub fn expire(&mut self, max_age: Duration) {
        let now = Instant::now();
        self.pending.retain(|_, (_, t)| now.duration_since(*t) < max_age);
        self.queued
            .retain(|_, t| now.duration_since(*t) < Duration::from_millis(500));
    }
}

impl Default for FastCancelCache {
    fn default() -> Self {
        Self::new()
    }
}
