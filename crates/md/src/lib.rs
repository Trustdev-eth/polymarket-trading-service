//! Market-data adapters. They emit `mm_engine::Event` — never call the engine themselves.

use anyhow::{Context, Result};
use mm_clob::{u256_from_dec, CLOB_HOST};
use mm_engine::{ContractInfo, Event};
use serde_json::Value;
use std::time::Duration;

/// Binance USDM perps is the latency Ŝ feed. Spot / vision are geo-block fallbacks.
pub const BINANCE_USDM_WS: &str = "wss://fstream.binance.com/ws";
pub const BINANCE_SPOT_WS: &str = "wss://stream.binance.com:443/ws";
pub const BINANCE_VISION_WS: &str = "wss://data-stream.binance.vision/ws";
pub const POLY_MARKET_WS: &str = "wss://ws-subscriptions-clob.polymarket.com/ws/market";
pub const POLY_USER_WS: &str = "wss://ws-subscriptions-clob.polymarket.com/ws/user";
pub const RTDS_WS: &str = "wss://ws-live-data.polymarket.com";
pub const GAMMA_URL: &str = "https://gamma-api.polymarket.com";
pub const CRYPTO_PRICE_URL: &str = "https://polymarket.com/api/crypto/crypto-price";
pub const RTDS_TWAP_TOPIC: &str = "crypto_prices_twap_sixty";
pub const RTDS_CHAINLINK_TOPIC: &str = "crypto_prices_chainlink";
/// Polymarket crypto taker delay (changelog: 150ms since 2026-09-04 14:00 UTC). The CLOB only
/// says whether it applies (`itode`), not how long, so up/down markets assume it until told otherwise.
pub const POLY_CRYPTO_TAKER_DELAY_MS: u64 = 150;

#[derive(Clone, Debug)]
pub struct ResolvedMarket {
    pub info: ContractInfo,
    pub yes_id: [u8; 32],
    pub no_id: [u8; 32],
    pub yes_id_dec: String,
    pub no_id_dec: String,
    pub market_id: String,
    pub slug: String,
}

/// Gamma slug `{base}-updown-{mins}m-{unix_s}` → yes/no ids, window, strike if present.
pub async fn fetch_gamma_contract(base: &str, mins: u32, unix_s: i64) -> Result<ResolvedMarket> {
    let slug = format!("{base}-updown-{mins}m-{unix_s}");
    fetch_gamma_slug(&slug, mins).await
}

pub async fn fetch_gamma_slug(slug: &str, mins: u32) -> Result<ResolvedMarket> {
    tracing::info!(%slug, "fetching gamma");
    let url = format!("{GAMMA_URL}/events?slug={slug}");
    let v: Value = http_get_json(&url).await?;
    tracing::info!(%slug, "gamma ok");
    let event = v
        .as_array()
        .and_then(|a| a.first())
        .cloned()
        .or_else(|| if v.is_object() { Some(v.clone()) } else { None })
        .context("gamma empty")?;
    let markets = event
        .get("markets")
        .and_then(|m| m.as_array())
        .context("no markets")?;
    let m = markets.first().context("no market")?;
    let clob_ids = parse_clob_ids(m)?;
    let yes_id_dec = clob_ids[0].clone();
    let no_id_dec = clob_ids[1].clone();
    let yes_id = u256_from_dec(&yes_id_dec).map_err(|e| anyhow::anyhow!(e))?;
    let no_id = u256_from_dec(&no_id_dec).map_err(|e| anyhow::anyhow!(e))?;
    let condition = m
        .get("conditionId")
        .or_else(|| m.get("condition_id"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let (start, end) = market_window(&event, m, mins);
    let k_t = parse_k_t(m, &event);

    Ok(ResolvedMarket {
        info: ContractInfo {
            utc0_ms: start,
            utc1_ms: end,
            k_t,
            fee_bps: 0,
            slug: slug.to_string(),
            tick_size: 0.01,
            neg_risk: false,
            taker_delay_ms: POLY_CRYPTO_TAKER_DELAY_MS,
        },
        yes_id,
        no_id,
        yes_id_dec,
        no_id_dec,
        market_id: condition,
        slug: slug.to_string(),
    })
}

/// Strike K for the live window. Gamma does not publish it; this is the open TWAP.
pub async fn fetch_open_strike(symbol: &str, utc0_ms: u64, utc1_ms: u64, mins: u32) -> Result<f64> {
    Ok(fetch_crypto_window(symbol, utc0_ms, utc1_ms, mins).await?.open)
}

#[derive(Clone, Debug)]
pub struct CryptoWindow {
    pub open: f64,
    pub close: Option<f64>,
    pub incomplete: bool,
}

pub async fn fetch_crypto_window(
    symbol: &str,
    utc0_ms: u64,
    utc1_ms: u64,
    mins: u32,
) -> Result<CryptoWindow> {
    let variant = if mins == 15 { "fifteen" } else { "fiveminute" };
    let mut url = url::Url::parse(CRYPTO_PRICE_URL).context("crypto-price url")?;
    url.query_pairs_mut()
        .append_pair("symbol", &symbol.to_uppercase())
        .append_pair("eventStartTime", &rfc3339_z(utc0_ms))
        .append_pair("variant", variant)
        .append_pair("endDate", &rfc3339_z(utc1_ms));
    let v: Value = http_get_json(url.as_str()).await?;
    parse_crypto_window(&v).context("crypto-price missing openPrice")
}

pub fn parse_crypto_window(v: &Value) -> Option<CryptoWindow> {
    let open = json_f64(v.get("openPrice")?)?;
    if open <= 0.0 {
        return None;
    }
    let close = v.get("closePrice").and_then(json_f64).filter(|c| *c > 0.0);
    let incomplete = v
        .get("incomplete")
        .and_then(|x| x.as_bool())
        .unwrap_or(close.is_none());
    Some(CryptoWindow {
        open,
        close,
        incomplete,
    })
}

pub fn parse_open_price(v: &Value) -> Option<f64> {
    parse_crypto_window(v).map(|w| w.open)
}

fn json_f64(x: &Value) -> Option<f64> {
    x.as_f64()
        .or_else(|| x.as_i64().map(|n| n as f64))
        .or_else(|| x.as_str().and_then(|s| s.parse().ok()))
}

fn parse_k_t(m: &Value, event: &Value) -> f64 {
    const KEYS: &[&str] = &["priceToBeat", "openPrice", "strikePrice", "targetPrice"];
    for src in [m, event] {
        for k in KEYS {
            if let Some(v) = src.get(*k).and_then(json_f64) {
                if v > 0.0 {
                    return v;
                }
            }
        }
    }
    if let Some(cfg) = m.get("cryptoMarketConfig") {
        for k in KEYS {
            if let Some(v) = cfg.get(*k).and_then(json_f64) {
                if v > 0.0 {
                    return v;
                }
            }
        }
    }
    0.0
}

fn market_window(event: &Value, m: &Value, mins: u32) -> (u64, u64) {
    let end = ["endDate", "endDateIso"]
        .iter()
        .find_map(|k| {
            m.get(*k)
                .or_else(|| event.get(*k))
                .and_then(|x| x.as_str())
                .and_then(parse_iso_ms)
        })
        .unwrap_or(0);
    let start = ["eventStartTime", "startTime"]
        .iter()
        .find_map(|k| {
            m.get(*k)
                .or_else(|| event.get(*k))
                .and_then(|x| x.as_str())
                .and_then(parse_iso_ms)
        })
        .unwrap_or(end.saturating_sub(u64::from(mins) * 60_000));
    let end = if end == 0 && start > 0 {
        start + u64::from(mins) * 60_000
    } else {
        end
    };
    (start, end)
}

fn rfc3339_z(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400) as u32;
    let (y, mo, d) = civil_from_days(days);
    let h = sod / 3600;
    let mi = (sod % 3600) / 60;
    let s = sod % 60;
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// Inverse of `days_from_civil` (Howard Hinnant). `z` is days since Unix epoch.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let mut y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    y += i64::from(m <= 2);
    (y, m, d)
}

fn parse_clob_ids(m: &Value) -> Result<Vec<String>> {
    if let Some(s) = m.get("clobTokenIds").and_then(|x| x.as_str()) {
        let v: Vec<String> = serde_json::from_str(s).context("clobTokenIds json")?;
        anyhow::ensure!(v.len() >= 2, "need yes/no ids");
        return Ok(v);
    }
    if let Some(a) = m.get("clobTokenIds").and_then(|x| x.as_array()) {
        let v: Vec<String> = a
            .iter()
            .filter_map(|x| x.as_str().map(|s| s.to_string()))
            .collect();
        anyhow::ensure!(v.len() >= 2, "need yes/no ids");
        return Ok(v);
    }
    anyhow::bail!("clobTokenIds missing")
}

fn parse_iso_ms(s: &str) -> Option<u64> {
    if s.is_empty() {
        return None;
    }
    // RFC3339-ish: 2026-03-09T16:10:00Z or with offset
    let s = s.trim();
    let (date, rest) = s.split_once('T')?;
    let mut ds = date.split('-');
    let y: i64 = ds.next()?.parse().ok()?;
    let mo: u32 = ds.next()?.parse().ok()?;
    let d: u32 = ds.next()?.parse().ok()?;
    let rest = rest.trim_end_matches('Z');
    let (hms, off_secs) = if let Some(i) = rest.find('+') {
        (&rest[..i], parse_hhmm_offset(&rest[i + 1..])?)
    } else if let Some(i) = rest.rfind('-') {
        if i > 2 {
            (&rest[..i], -parse_hhmm_offset(&rest[i + 1..])?)
        } else {
            (rest, 0i64)
        }
    } else {
        (rest, 0i64)
    };
    let mut ts = hms.split(':');
    let h: u32 = ts.next()?.parse().ok()?;
    let mi: u32 = ts.next()?.parse().ok()?;
    let se: f64 = ts.next().unwrap_or("0").parse().unwrap_or(0.0);
    let days = days_from_civil(y, mo, d)?;
    let unix = days * 86400 + i64::from(h) * 3600 + i64::from(mi) * 60 + se as i64 - off_secs;
    Some((unix as u64) * 1000)
}

fn parse_hhmm_offset(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return Some(0);
    }
    let (h, m) = if let Some((a, b)) = s.split_once(':') {
        (a.parse::<i64>().ok()?, b.parse::<i64>().unwrap_or(0))
    } else if s.len() >= 4 {
        (s[..2].parse().ok()?, s[2..4].parse().ok()?)
    } else {
        (s.parse::<i64>().ok()?, 0)
    };
    Some(h * 3600 + m * 60)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> Option<i64> {
    // Howard Hinnant civil_from_days inverse
    let mut y = y;
    let m = m as i64;
    let d = d as i64;
    if m <= 2 {
        y -= 1;
    }
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146097 + doe - 719468)
}

pub fn http_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .no_proxy()
        .tcp_nodelay(true)
        .tcp_keepalive(Duration::from_secs(15))
        .pool_max_idle_per_host(8)
        .pool_idle_timeout(Duration::from_secs(90))
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(8))
        .build()?)
}

/// GET JSON without tokio sockets on Windows (mio AFD ACCESS_VIOLATION).
async fn http_get_json(url: &str) -> Result<Value> {
    #[cfg(windows)]
    {
        let url = url.to_string();
        tokio::task::spawn_blocking(move || {
            let agent = win_ureq_agent()?;
            let resp = agent
                .get(&url)
                .call()
                .map_err(|e| anyhow::anyhow!("GET {url}: {e}"))?;
            resp.into_json()
                .map_err(|e| anyhow::anyhow!("GET {url} json: {e}"))
        })
        .await?
    }
    #[cfg(not(windows))]
    {
        let http = http_client()?;
        Ok(http.get(url).send().await?.json().await?)
    }
}

#[cfg(windows)]
fn win_ureq_agent() -> Result<ureq::Agent> {
    let tls = native_tls::TlsConnector::new()?;
    Ok(ureq::AgentBuilder::new()
        .tls_connector(std::sync::Arc::new(tls))
        .timeout(Duration::from_secs(8))
        .build())
}

/// TCP_NODELAY + keepalive + Linux TCP_QUICKACK on the strategy↔gateway socket.
pub fn tune_tcp(stream: &tokio::net::TcpStream) {
    let _ = stream.set_nodelay(true);
    let sock = socket2::SockRef::from(stream);
    let ka = socket2::TcpKeepalive::new()
        .with_time(Duration::from_secs(10))
        .with_interval(Duration::from_secs(3));
    #[cfg(target_os = "linux")]
    let ka = ka.with_retries(3);
    let _ = sock.set_tcp_keepalive(&ka);
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let _ = sock.set_quickack(true);
    }
}

pub async fn current_bucket_unix_s(mins: u32) -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let span = i64::from(mins) * 60;
    (now / span) * span
}

/// Pre-resolve tick/fee/neg-risk once per contract (not on order path).
pub async fn resolve_clob_meta(market: &mut ResolvedMarket) -> Result<()> {
    let (meta, delay) = tokio::join!(
        mm_clob::fetch_tick_fee_negrisk(CLOB_HOST, &market.yes_id_dec),
        mm_clob::fetch_match_delay(CLOB_HOST, &market.market_id),
    );
    let (tick, fee, nr) = meta.map_err(anyhow::Error::msg)?;
    market.info.tick_size = tick;
    market.info.fee_bps = fee;
    market.info.neg_risk = nr;
    match delay {
        Ok((itode, seconds)) => {
            market.info.taker_delay_ms =
                if itode { POLY_CRYPTO_TAKER_DELAY_MS } else { 0 } + seconds * 1_000;
        }
        Err(e) => tracing::warn!(error=%e, "match delay lookup failed; assuming crypto taker delay"),
    }
    Ok(())
}

pub mod binance;
pub mod poly_book;
pub mod rtds;
pub mod user;
#[cfg(windows)]
mod win_ws;

/// Feeds push straight into the engine thread's queue. Unbounded: a send never blocks a runtime worker.
pub type EventTx = crossbeam_channel::Sender<DatedEvent>;

#[derive(Clone, Debug)]
pub struct DatedEvent {
    pub recv_ns: u64,
    pub event: Event,
}

pub fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_unix_known() {
        // 2026-03-09T16:10:00Z = 1773072600
        let ms = parse_iso_ms("2026-03-09T16:10:00Z").unwrap();
        assert_eq!(ms, 1_773_072_600_000);
        let zoned = parse_iso_ms("2026-03-09T12:10:00-04:00").unwrap();
        assert_eq!(zoned, 1_773_072_600_000);
    }

    #[test]
    fn rfc3339_roundtrip() {
        let ms = 1_790_438_400_000;
        assert_eq!(rfc3339_z(ms), "2026-09-26T16:00:00Z");
        assert_eq!(parse_iso_ms(&rfc3339_z(ms)).unwrap(), ms);
    }

    #[test]
    fn parse_open_price_sample() {
        let v: Value = serde_json::from_str(
            r#"{"openPrice":84138.85057415086,"closePrice":84140.1,"incomplete":false}"#,
        )
        .unwrap();
        let w = parse_crypto_window(&v).unwrap();
        assert!((w.open - 84138.85057415086).abs() < 1e-6);
        assert!((w.close.unwrap() - 84140.1).abs() < 1e-6);
        assert!(!w.incomplete);
        let p = parse_open_price(&v).unwrap();
        assert!((p - 84138.85057415086).abs() < 1e-6);
    }

    #[test]
    fn market_window_prefers_event_start_not_created_at() {
        let event = serde_json::json!({
            "startDate": "2026-09-25T16:09:42.888666Z",
            "startTime": "2026-09-26T16:00:00Z",
            "endDate": "2026-09-26T16:05:00Z"
        });
        let m = serde_json::json!({
            "eventStartTime": "2026-09-26T16:00:00Z",
            "endDate": "2026-09-26T16:05:00Z"
        });
        let (start, end) = market_window(&event, &m, 5);
        assert_eq!(start, 1_790_438_400_000);
        assert_eq!(end, 1_790_438_700_000);
    }

    #[test]
    fn fifteen_minute_window_when_end_missing() {
        let event = serde_json::json!({ "startTime": "2026-09-26T16:00:00Z" });
        let m = serde_json::json!({ "eventStartTime": "2026-09-26T16:00:00Z" });
        let (start, end) = market_window(&event, &m, 15);
        assert_eq!(start, 1_790_438_400_000);
        assert_eq!(end - start, 900_000);
    }

    #[test]
    fn fifteen_variant_slug() {
        assert_eq!(
            format!("{}-updown-{}m-{}", "btc", 15, 1_773_072_600),
            "btc-updown-15m-1773072600"
        );
    }
}
