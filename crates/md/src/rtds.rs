use anyhow::Result;
#[cfg(not(windows))]
use futures_util::{SinkExt, StreamExt};
use mm_engine::Event;
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(not(windows))]
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::{now_ns, DatedEvent, EventTx, RTDS_CHAINLINK_TOPIC, RTDS_TWAP_TOPIC, RTDS_WS};

static LAST_CHAINLINK_MS: AtomicU64 = AtomicU64::new(0);
static LAST_TWAP_MS: AtomicU64 = AtomicU64::new(0);

/// Age of the last Polymarket RTDS Chainlink print. `u64::MAX` if none yet.
pub fn chainlink_age_ms(now_ms: u64) -> u64 {
    age_since(LAST_CHAINLINK_MS.load(Ordering::Relaxed), now_ms)
}

/// Age of the last Polymarket RTDS 60s TWAP print. `u64::MAX` if none yet.
pub fn twap_age_ms(now_ms: u64) -> u64 {
    age_since(LAST_TWAP_MS.load(Ordering::Relaxed), now_ms)
}

fn age_since(last: u64, now_ms: u64) -> u64 {
    if last == 0 {
        u64::MAX
    } else {
        now_ms.saturating_sub(last)
    }
}

fn mark(slot: &AtomicU64) {
    slot.store(now_ns() / 1_000_000, Ordering::Relaxed);
}

const PING_S: u64 = 5;
const IDLE_S: u64 = 8;

fn subscribe_json(symbol: &str) -> String {
    let filters = format!(r#"{{"symbol":"{symbol}/usd"}}"#);
    serde_json::json!({
        "action": "subscribe",
        "subscriptions": [
            {
                "topic": RTDS_TWAP_TOPIC,
                "type": "update",
                "filters": filters
            },
            {
                "topic": RTDS_CHAINLINK_TOPIC,
                "type": "update",
                "filters": filters
            }
        ]
    })
    .to_string()
}

/// Instant Chainlink (fair S) + 60s TWAP (K at open, lock-in at end).
pub async fn run_twap(symbol: &str, tx: EventTx) -> Result<()> {
    let want = symbol.to_lowercase();
    loop {
        #[cfg(windows)]
        {
            let want_c = want.clone();
            let tx_c = tx.clone();
            let res = tokio::task::spawn_blocking(move || stream_once_std(&want_c, &tx_c)).await?;
            if let Err(e) = res {
                tracing::warn!(error=%e, "rtds twap");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
        #[cfg(not(windows))]
        if let Err(e) = stream_once(&want, &tx).await {
            tracing::warn!(error=%e, "rtds twap");
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }
}

#[cfg(windows)]
fn stream_once_std(symbol: &str, tx: &EventTx) -> Result<()> {
    let sub = subscribe_json(symbol);
    tracing::info!("rtds subscribed twap+chainlink");
    let sym = symbol.to_string();
    crate::win_ws::connect_text_loop(
        RTDS_WS,
        Some(sub),
        None,
        std::time::Duration::from_secs(IDLE_S),
        |txt| {
            if let Some(ev) = parse_rtds(txt, &sym) {
                let _ = tx.send(DatedEvent {
                    recv_ns: now_ns(),
                    event: ev,
                });
            }
        },
    )
}

#[cfg(not(windows))]
async fn stream_once(symbol: &str, tx: &EventTx) -> Result<()> {
    let (ws, _) = connect_async(RTDS_WS).await?;
    let (mut sink, mut read) = ws.split();
    sink.send(Message::Text(subscribe_json(symbol).into())).await?;
    tracing::info!("rtds subscribed twap+chainlink");
    let mut ping = tokio::time::interval(std::time::Duration::from_secs(PING_S));
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut idle = tokio::time::interval(std::time::Duration::from_secs(IDLE_S));
    idle.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    idle.tick().await;
    loop {
        tokio::select! {
            msg = read.next() => {
                let Some(msg) = msg else {
                    anyhow::bail!("rtds ws closed");
                };
                let txt = match msg? {
                    Message::Text(t) => t,
                    Message::Ping(p) => {
                        let _ = sink.send(Message::Pong(p)).await;
                        continue;
                    }
                    Message::Pong(_) => continue,
                    Message::Close(_) => anyhow::bail!("rtds ws close frame"),
                    _ => continue,
                };
                if txt.eq_ignore_ascii_case("pong") || txt.eq_ignore_ascii_case("ping") || txt.is_empty() {
                    continue;
                }
                if let Some(ev) = parse_rtds(&txt, symbol) {
                    idle.reset();
                    let _ = tx.send(DatedEvent {
                        recv_ns: now_ns(),
                        event: ev,
                    });
                }
            }
            _ = ping.tick() => {
                // RTDS app heartbeat is the text frame "PING", not a WS ping.
                sink.send(Message::Text("PING".into())).await?;
            }
            _ = idle.tick() => {
                anyhow::bail!("rtds idle {IDLE_S}s (no {symbol} chainlink/twap)");
            }
        }
    }
}

pub fn parse_twap(txt: &str, symbol: &str) -> Option<Event> {
    parse_rtds(txt, symbol)
}

pub fn parse_rtds(txt: &str, symbol: &str) -> Option<Event> {
    let v: serde_json::Value = serde_json::from_str(txt).ok()?;
    let topic = v.get("topic").and_then(|x| x.as_str()).unwrap_or("");
    let payload = v.get("payload")?;
    let sym = payload
        .get("symbol")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_lowercase();
    if !sym.is_empty() && !sym.contains(symbol) && format!("{symbol}/usd") != sym {
        return None;
    }
    let ts = payload
        .get("timestamp")
        .and_then(|x| x.as_u64())
        .or_else(|| v.get("timestamp").and_then(|x| x.as_u64()))?;
    let value = payload.get("value")?.as_f64().or_else(|| {
        payload
            .get("value")
            .and_then(|x| x.as_str())
            .and_then(|s| s.parse().ok())
    })?;
    match topic {
        t if t == RTDS_TWAP_TOPIC => {
            mark(&LAST_TWAP_MS);
            Some(Event::Twap { ts_ms: ts, value })
        }
        t if t == RTDS_CHAINLINK_TOPIC => {
            mark(&LAST_CHAINLINK_MS);
            Some(Event::Chainlink { ts_ms: ts, value })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sample() {
        let s = r#"{"topic":"crypto_prices_twap_sixty","timestamp":1,"payload":{"symbol":"btc/usd","timestamp":1773072600000,"value":68825.91}}"#;
        match parse_rtds(s, "btc") {
            Some(Event::Twap { value, ts_ms }) => {
                assert!((value - 68825.91).abs() < 1e-6);
                assert_eq!(ts_ms, 1773072600000);
            }
            _ => panic!("parse"),
        }
    }

    #[test]
    fn parse_chainlink_spot() {
        let s = r#"{"topic":"crypto_prices_chainlink","timestamp":1,"payload":{"symbol":"btc/usd","timestamp":1773072600000,"value":69001.5}}"#;
        match parse_rtds(s, "btc") {
            Some(Event::Chainlink { value, ts_ms }) => {
                assert!((value - 69001.5).abs() < 1e-6);
                assert_eq!(ts_ms, 1773072600000);
            }
            _ => panic!("parse chainlink"),
        }
    }
}
