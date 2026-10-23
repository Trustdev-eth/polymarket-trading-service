//! REST overlays when Binance / Polymarket / RTDS websockets are geo-blocked or hung.
//! Dashboard and the coordinator both read this. No private keys.
//!
//! Chainlink is Coinbase/Kraken USD spot (RTDS `crypto_prices_chainlink` fallback).
//! TWAP is a local 60s time-weighted average of those Chainlink prints — never window
//! open/K, and never a copy of the Binance mid.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use mm_engine::{BookDepth, Event, Level, DEPTH_LEVELS};
use mm_md::{now_ns, DatedEvent, EventTx};
use reqwest::Client;
use serde_json::Value;
use tokio::sync::Mutex;

const BINANCE_REST: &[&str] = &[
    "https://data-api.binance.vision/api/v3/ticker/bookTicker?symbol=",
    "https://api.binance.com/api/v3/ticker/bookTicker?symbol=",
    "https://fapi.binance.com/fapi/v1/ticker/bookTicker?symbol=",
];

const TWAP_MS: u64 = 60_000;
const COPY_EPS: f64 = 1e-6;

#[derive(Clone, Debug, Default)]
pub struct Overlay {
    pub slug: String,
    pub ts_ms: i64,
    pub binance: f64,
    pub chainlink: f64,
    pub twap: f64,
    pub s_hat: f64,
    pub yes_bid: f64,
    pub yes_ask: f64,
    pub no_bid: f64,
    pub no_ask: f64,
    pub tau_s: f64,
    pub k_t: f64,
    pub utc0_ms: i64,
    pub utc1_ms: i64,
    pub yes_id: String,
    pub no_id: String,
}

struct Cache {
    at: Instant,
    overlay: Overlay,
}

#[derive(Default)]
struct TwapAcc {
    samples: VecDeque<(u64, f64)>,
}

impl TwapAcc {
    fn push(&mut self, ts_ms: u64, px: f64) -> f64 {
        if px > 0.0 {
            if self
                .samples
                .back()
                .is_none_or(|&(t, last)| ts_ms > t || (ts_ms == t && (last - px).abs() > COPY_EPS))
            {
                self.samples.push_back((ts_ms, px));
            }
        }
        let floor = ts_ms.saturating_sub(TWAP_MS);
        while self.samples.front().is_some_and(|&(t, _)| t < floor) {
            self.samples.pop_front();
        }
        rolling_twap(&self.samples, ts_ms, px)
    }
}

fn rolling_twap(samples: &VecDeque<(u64, f64)>, now_ms: u64, fallback: f64) -> f64 {
    if samples.is_empty() {
        return fallback;
    }
    let start = now_ms.saturating_sub(TWAP_MS);
    let mut num = 0.0;
    let mut den = 0.0;
    for i in 0..samples.len() {
        let (t, px) = samples[i];
        let t0 = t.max(start);
        let t1 = if i + 1 < samples.len() {
            samples[i + 1].0
        } else {
            now_ms.max(t0)
        };
        if t1 > t0 {
            let dt = (t1 - t0) as f64;
            num += px * dt;
            den += dt;
        }
    }
    if den > 0.0 {
        num / den
    } else {
        samples.back().map(|s| s.1).unwrap_or(fallback)
    }
}

static CACHE: Mutex<Option<Cache>> = Mutex::const_new(None);
static TWAP: Mutex<TwapAcc> = Mutex::const_new(TwapAcc {
    samples: VecDeque::new(),
});

fn copied(a: f64, b: f64) -> bool {
    a > 0.0 && b > 0.0 && (a - b).abs() <= COPY_EPS
}

async fn note_chainlink(ts_ms: u64, px: f64) -> f64 {
    let mut g = TWAP.lock().await;
    g.push(ts_ms, px)
}

pub fn parse_book_ticker_rest(v: &Value) -> Option<(f64, f64)> {
    let bid = json_f64(v.get("bidPrice")).or_else(|| json_f64(v.get("b")))?;
    let ask = json_f64(v.get("askPrice")).or_else(|| json_f64(v.get("a")))?;
    if bid > 0.0 && ask >= bid {
        Some((bid, ask))
    } else {
        None
    }
}

pub fn parse_coinbase_ticker(v: &Value) -> Option<f64> {
    let bid = json_f64(v.get("bid"));
    let ask = json_f64(v.get("ask"));
    match (bid, ask) {
        (Some(b), Some(a)) if b > 0.0 && a >= b => Some(0.5 * (b + a)),
        _ => json_f64(v.get("price")),
    }
}

pub fn parse_kraken_ticker(v: &Value) -> Option<f64> {
    let pair = v.get("result")?.as_object()?.values().next()?;
    let first = |key: &str| {
        pair.get(key)
            .and_then(|x| x.as_array())
            .and_then(|a| a.first())
            .and_then(|x| json_f64(Some(x)))
    };
    match (first("b"), first("a")) {
        (Some(b), Some(a)) if b > 0.0 && a >= b => Some(0.5 * (b + a)),
        _ => first("c"),
    }
}

pub fn parse_clob_book(v: &Value) -> Option<(f64, f64, BookDepth)> {
    let ts = json_f64(v.get("timestamp")).unwrap_or(0.0) as u64;
    let ts = if ts > 0 { ts } else { now_ns() / 1_000_000 };
    let mut bids = levels(v.get("bids"));
    let mut asks = levels(v.get("asks"));
    bids.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    asks.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let (yes_bid, bid_sz) = *bids.first()?;
    let (yes_ask, ask_sz) = *asks.first()?;
    if yes_bid <= 0.0 || yes_ask <= 0.0 {
        return None;
    }
    let mut depth = BookDepth {
        ts_ms: ts,
        ..BookDepth::default()
    };
    fill_depth(&mut depth.bids, &bids);
    fill_depth(&mut depth.asks, &asks);
    let _ = (bid_sz, ask_sz);
    Some((yes_bid, yes_ask, depth))
}

fn fill_depth(dst: &mut [Level; DEPTH_LEVELS], src: &[(f64, f64)]) {
    for (slot, &(px, size)) in dst.iter_mut().zip(src.iter()) {
        *slot = Level { px, size };
    }
}

fn levels(arr: Option<&Value>) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    for lvl in arr.and_then(|a| a.as_array()).into_iter().flatten() {
        let Some(px) = json_f64(lvl.get("price")) else {
            continue;
        };
        let Some(size) = json_f64(lvl.get("size")) else {
            continue;
        };
        if px > 0.0 && size > 0.0 {
            out.push((px, size));
        }
    }
    out
}

fn json_f64(x: Option<&Value>) -> Option<f64> {
    let x = x?;
    x.as_f64()
        .or_else(|| x.as_i64().map(|n| n as f64))
        .or_else(|| x.as_u64().map(|n| n as f64))
        .or_else(|| x.as_str().and_then(|s| s.trim().parse().ok()))
        .filter(|n| n.is_finite() && *n > 0.0)
}

async fn http_json(http: &Client, url: &str) -> Result<Value> {
    let res = http
        .get(url)
        .timeout(Duration::from_secs(6))
        .send()
        .await
        .with_context(|| url.to_string())?;
    let status = res.status();
    let v: Value = res.json().await.with_context(|| url.to_string())?;
    if !status.is_success() {
        anyhow::bail!("{url} HTTP {status} {v}");
    }
    Ok(v)
}

pub async fn binance_bba(http: &Client, symbol: &str) -> Result<(f64, f64)> {
    let sym = symbol.to_uppercase();
    let mut last = anyhow::anyhow!("no binance rest host");
    for host in BINANCE_REST {
        let url = format!("{host}{sym}");
        match http_json(http, &url).await {
            Ok(v) => {
                if let Some(pair) = parse_book_ticker_rest(&v) {
                    return Ok(pair);
                }
                last = anyhow::anyhow!("{url} missing bid/ask");
            }
            Err(e) => last = e,
        }
    }
    Err(last)
}

pub async fn chainlink_spot(http: &Client, base: &str) -> Result<f64> {
    let asset = base.trim().to_uppercase();
    let cb = format!("https://api.exchange.coinbase.com/products/{asset}-USD/ticker");
    if let Ok(v) = http_json(http, &cb).await {
        if let Some(px) = parse_coinbase_ticker(&v) {
            return Ok(px);
        }
    }
    let pair = if asset == "BTC" {
        "XBTUSD".to_string()
    } else {
        format!("{asset}USD")
    };
    let kr = format!("https://api.kraken.com/0/public/Ticker?pair={pair}");
    let v = http_json(http, &kr).await?;
    parse_kraken_ticker(&v).context("kraken ticker empty")
}

pub async fn clob_book(http: &Client, yes_id: &str) -> Result<(f64, f64, BookDepth)> {
    let url = format!(
        "{}/book?token_id={yes_id}",
        mm_clob::CLOB_HOST.trim_end_matches('/')
    );
    let v = http_json(http, &url).await?;
    parse_clob_book(&v).context("clob book empty")
}

pub async fn current_market(base: &str, mins: u32) -> Result<mm_md::ResolvedMarket> {
    let unix = mm_md::current_bucket_unix_s(mins).await;
    let mut market = mm_md::fetch_gamma_contract(base, mins, unix).await?;
    if market.info.k_t <= 0.0 {
        if let Ok(k) = mm_md::fetch_open_strike(base, market.info.utc0_ms, market.info.utc1_ms, mins).await
        {
            if k > 0.0 {
                market.info.k_t = k;
            }
        }
    }
    Ok(market)
}

async fn overlay_now(http: &Client, base: &str, mins: u32, binance_sym: &str) -> Result<Overlay> {
    let market = current_market(base, mins).await?;
    let now = crate::db::now_ms();
    let mut out = Overlay {
        slug: market.slug.clone(),
        ts_ms: now,
        k_t: market.info.k_t,
        utc0_ms: market.info.utc0_ms as i64,
        utc1_ms: market.info.utc1_ms as i64,
        tau_s: if market.info.utc1_ms > now as u64 {
            (market.info.utc1_ms - now as u64) as f64 / 1000.0
        } else {
            0.0
        },
        yes_id: market.yes_id_dec.clone(),
        no_id: market.no_id_dec.clone(),
        ..Overlay::default()
    };
    let (bn, cl, book) = tokio::join!(
        binance_bba(http, binance_sym),
        chainlink_spot(http, base),
        clob_book(http, &market.yes_id_dec),
    );
    if let Ok((bid, ask)) = bn {
        let mid = 0.5 * (bid + ask);
        out.binance = mid;
        out.s_hat = mid;
    }
    if let Ok(px) = cl {
        out.chainlink = px;
        out.twap = note_chainlink(now as u64, px).await;
    }
    if let Ok((yb, ya, _)) = book {
        out.yes_bid = yb;
        out.yes_ask = ya;
        out.no_bid = (1.0 - ya).max(0.0);
        out.no_ask = (1.0 - yb).max(0.0);
    }
    Ok(out)
}

pub async fn cached_overlay(http: &Client, base: &str, mins: u32, binance_sym: &str) -> Result<Overlay> {
    {
        let g = CACHE.lock().await;
        if let Some(c) = g.as_ref() {
            if c.at.elapsed() < Duration::from_millis(750)
                && (c.overlay.binance > 0.0 || c.overlay.chainlink > 0.0)
            {
                return Ok(c.overlay.clone());
            }
        }
    }
    let overlay = overlay_now(http, base, mins, binance_sym).await?;
    let mut g = CACHE.lock().await;
    *g = Some(Cache {
        at: Instant::now(),
        overlay: overlay.clone(),
    });
    Ok(overlay)
}

pub fn merge_tick(tick: &mut serde_json::Value, o: &Overlay) {
    let obj = match tick.as_object_mut() {
        Some(m) => m,
        None => return,
    };
    let cur = |obj: &serde_json::Map<String, Value>, k: &str| {
        obj.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0)
    };
    let set_if = |obj: &mut serde_json::Map<String, Value>, k: &str, n: f64| {
        if n > 0.0 && cur(obj, k) <= 0.0 {
            obj.insert(k.into(), serde_json::json!(n));
        }
    };
    if !o.slug.is_empty() {
        obj.insert("slug".into(), serde_json::json!(o.slug));
    }
    obj.insert("ts".into(), serde_json::json!(o.ts_ms));
    set_if(obj, "binance", o.binance);
    set_if(obj, "s_hat", o.s_hat);
    set_if(obj, "yes_bid", o.yes_bid);
    set_if(obj, "yes_ask", o.yes_ask);
    set_if(obj, "no_bid", o.no_bid);
    set_if(obj, "no_ask", o.no_ask);
    set_if(obj, "k_t", o.k_t);
    let binance = cur(obj, "binance");
    let k_t = cur(obj, "k_t");
    let chainlink = cur(obj, "chainlink");
    if o.chainlink > 0.0 && (chainlink <= 0.0 || copied(chainlink, binance)) {
        obj.insert("chainlink".into(), serde_json::json!(o.chainlink));
    }
    let twap = cur(obj, "twap");
    if o.twap > 0.0 && (twap <= 0.0 || copied(twap, k_t) || copied(twap, binance)) {
        obj.insert("twap".into(), serde_json::json!(o.twap));
    }
    if o.tau_s > 0.0 {
        obj.insert("tau_s".into(), serde_json::json!(o.tau_s));
    }
    if o.utc0_ms > 0 {
        obj.insert("utc0_ms".into(), serde_json::json!(o.utc0_ms));
        obj.insert("utc1_ms".into(), serde_json::json!(o.utc1_ms));
    }
}

/// Overlay zeros, stale snapshots, and the old copy bugs (Chainlink=Binance, TWAP=K).
pub fn tick_needs_overlay(tick: &serde_json::Value, now_ms: i64) -> bool {
    let n = |k: &str| tick.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0);
    let ts = tick.get("ts").and_then(|x| x.as_i64()).unwrap_or(0);
    let binance = n("binance");
    let chainlink = n("chainlink");
    let twap = n("twap");
    let k_t = n("k_t");
    let yes = n("yes_bid");
    binance <= 0.0
        || yes <= 0.0
        || chainlink <= 0.0
        || twap <= 0.0
        || copied(chainlink, binance)
        || copied(twap, k_t)
        || copied(twap, binance)
        || now_ms.saturating_sub(ts) > 8_000
}

pub async fn run_rest_feeds(
    base: String,
    mins: u32,
    binance_sym: String,
    mut assets_rx: tokio::sync::watch::Receiver<(String, String)>,
    tx: EventTx,
) {
    let http = Client::builder()
        .user_agent("ohioism-trade/0.1")
        .timeout(Duration::from_secs(6))
        .build();
    let Ok(http) = http else {
        tracing::warn!("rest md client");
        return;
    };
    tracing::info!("rest md overlay (binance + chainlink spot + 60s twap)");
    let mut last_warn = Instant::now() - Duration::from_secs(60);
    let mut latched_utc0 = 0u64;
    loop {
        let (yes, _) = assets_rx.borrow_and_update().clone();
        let now = now_ns() / 1_000_000;
        let unix = mm_md::current_bucket_unix_s(mins).await as u64;
        let utc0 = unix.saturating_mul(1000);
        let utc1 = utc0 + u64::from(mins) * 60_000;
        if latched_utc0 != utc0 {
            if let Ok(win) = mm_md::fetch_crypto_window(&base, utc0, utc1, mins).await {
                if win.open > 0.0 {
                    let _ = tx.send(DatedEvent {
                        recv_ns: now_ns(),
                        event: Event::Twap {
                            ts_ms: utc0 + 1_000,
                            value: win.open,
                        },
                    });
                    latched_utc0 = utc0;
                }
            }
        }
        if let Ok((bid, ask)) = binance_bba(&http, &binance_sym).await {
            let _ = tx.send(DatedEvent {
                recv_ns: now_ns(),
                event: Event::BinanceBba {
                    ts_exch_ms: now,
                    bid,
                    ask,
                },
            });
        } else if last_warn.elapsed() > Duration::from_secs(20) {
            tracing::warn!("binance rest still blocked");
            last_warn = Instant::now();
        }
        if let Ok(cl) = chainlink_spot(&http, &base).await {
            let _ = tx.send(DatedEvent {
                recv_ns: now_ns(),
                event: Event::Chainlink {
                    ts_ms: now,
                    value: cl,
                },
            });
            let tw = note_chainlink(now, cl).await;
            let in_lock = now + 2_000 >= utc0 && now <= utc0 + 5_000;
            if tw > 0.0 && (!in_lock || latched_utc0 == utc0) {
                let _ = tx.send(DatedEvent {
                    recv_ns: now_ns(),
                    event: Event::Twap {
                        ts_ms: now,
                        value: tw,
                    },
                });
            }
        }
        if !yes.is_empty() && yes != "0" {
            if let Ok((yb, ya, depth)) = clob_book(&http, &yes).await {
                let _ = tx.send(DatedEvent {
                    recv_ns: now_ns(),
                    event: Event::Book {
                        ts_ms: now,
                        yes_bid: yb,
                        yes_ask: ya,
                    },
                });
                let _ = tx.send(DatedEvent {
                    recv_ns: now_ns(),
                    event: Event::Depth(Box::new(depth)),
                });
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(500)) => {}
            r = assets_rx.changed() => {
                if r.is_err() {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_vision_ticker() {
        let v = json!({"symbol":"BTCUSDT","bidPrice":"81654.08","askPrice":"81654.09"});
        let (b, a) = parse_book_ticker_rest(&v).unwrap();
        assert!((b - 81654.08).abs() < 1e-9);
        assert!((a - 81654.09).abs() < 1e-9);
    }

    #[test]
    fn parses_coinbase_mid() {
        let v = json!({"bid":"81549.99","ask":"81550","price":"81550"});
        let px = parse_coinbase_ticker(&v).unwrap();
        assert!((px - 81549.995).abs() < 1e-9);
    }

    #[test]
    fn parses_kraken_mid() {
        let v = json!({"error":[],"result":{"XXBTZUSD":{
            "a":["81546.60000","1","1.000"],
            "b":["81546.50000","1","1.000"],
            "c":["81543.40000","0.0001"]
        }}});
        let px = parse_kraken_ticker(&v).unwrap();
        assert!((px - 81546.55).abs() < 1e-9);
    }

    #[test]
    fn rolling_twap_holds_first_print() {
        let mut a = TwapAcc::default();
        a.push(0, 100.0);
        let tw = a.push(60_000, 200.0);
        assert!((tw - 100.0).abs() < 1e-9);
    }

    #[test]
    fn rolling_twap_is_time_weighted() {
        let mut a = TwapAcc::default();
        a.push(0, 100.0);
        a.push(45_000, 200.0);
        let tw = a.push(60_000, 200.0);
        // 45s @ 100 + 15s @ 200 = 125
        assert!((tw - 125.0).abs() < 1e-9);
    }

    #[test]
    fn parses_clob_book_touch() {
        let v = json!({
            "timestamp": "100",
            "bids": [{"price":"0.40","size":"12"},{"price":"0.41","size":"3"}],
            "asks": [{"price":"0.44","size":"8"},{"price":"0.43","size":"5"}]
        });
        let (bid, ask, d) = parse_clob_book(&v).unwrap();
        assert!((bid - 0.41).abs() < 1e-9);
        assert!((ask - 0.43).abs() < 1e-9);
        assert!((d.bids[0].px - 0.41).abs() < 1e-9);
        assert!((d.asks[0].px - 0.43).abs() < 1e-9);
    }

    #[test]
    fn overlay_needed_when_prices_are_zero() {
        let t = json!({"binance":0.0,"yes_bid":0.0,"ts": 1});
        assert!(tick_needs_overlay(&t, 20_000));
    }

    #[test]
    fn overlay_needed_when_chainlink_copied_from_binance() {
        let t = json!({"binance":81650.0,"chainlink":81650.0,"twap":81640.0,"yes_bid":0.71,"k_t":81520.0,"ts": 19_000});
        assert!(tick_needs_overlay(&t, 20_000));
    }

    #[test]
    fn overlay_needed_when_twap_is_k() {
        let t = json!({"binance":81650.0,"chainlink":81550.0,"twap":81520.0,"yes_bid":0.71,"k_t":81520.0,"ts": 19_000});
        assert!(tick_needs_overlay(&t, 20_000));
    }

    #[test]
    fn merge_replaces_copied_chainlink_and_k_twap() {
        let mut tick = json!({
            "binance": 81650.0,
            "chainlink": 81650.0,
            "twap": 81520.0,
            "k_t": 81520.0,
            "yes_bid": 0.71,
            "ts": 1
        });
        let o = Overlay {
            ts_ms: 2,
            binance: 81651.0,
            chainlink: 81548.0,
            twap: 81547.0,
            k_t: 81520.0,
            ..Overlay::default()
        };
        merge_tick(&mut tick, &o);
        assert!((tick["chainlink"].as_f64().unwrap() - 81548.0).abs() < 1e-9);
        assert!((tick["twap"].as_f64().unwrap() - 81547.0).abs() < 1e-9);
        assert!((tick["binance"].as_f64().unwrap() - 81650.0).abs() < 1e-9);
        assert!((tick["k_t"].as_f64().unwrap() - 81520.0).abs() < 1e-9);
    }
}
