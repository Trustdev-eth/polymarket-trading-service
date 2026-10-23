use anyhow::Result;
use mm_engine::Event;
use serde::Deserialize;

use crate::{now_ns, DatedEvent, EventTx, BINANCE_SPOT_WS, BINANCE_USDM_WS, BINANCE_VISION_WS};

/// USDM first (speed Ŝ). Spot / vision only if perps is geo-blocked.
fn ws_urls(symbol: &str) -> [String; 3] {
    let s = symbol.to_lowercase();
    [
        format!("{BINANCE_USDM_WS}/{s}@bookTicker"),
        format!("{BINANCE_VISION_WS}/{s}@bookTicker"),
        format!("{BINANCE_SPOT_WS}/{s}@bookTicker"),
    ]
}

/// Binance `bookTicker` → `Event::BinanceBba` (leader for Ŝ, not settlement).
pub async fn run_book_ticker(symbol: &str, tx: EventTx) -> Result<()> {
    let urls = ws_urls(symbol);
    let mut idx = 0usize;
    loop {
        let url = &urls[idx];
        tracing::info!(%url, "binance usdm bookTicker connecting");
        #[cfg(windows)]
        {
            let url_c = url.clone();
            let tx_c = tx.clone();
            let res = tokio::task::spawn_blocking(move || {
                crate::win_ws::connect_text_loop(
                    &url_c,
                    None,
                    None,
                    std::time::Duration::from_secs(15),
                    |txt| {
                        if let Some(ev) = parse_book_ticker(txt) {
                            let _ = tx_c.send(DatedEvent {
                                recv_ns: now_ns(),
                                event: ev,
                            });
                        }
                    },
                )
            })
            .await?;
            match res {
                Ok(()) => tracing::warn!(url = %url, "binance ws closed; reconnecting"),
                Err(e) => {
                    let wait = reconnect_wait(&e);
                    tracing::warn!(url = %url, error=%e, secs = wait.as_secs(), "binance ws dropped");
                    if format!("{e:#}").contains("451") {
                        idx = (idx + 1) % urls.len();
                    }
                    tokio::time::sleep(wait).await;
                    continue;
                }
            }
        }
        #[cfg(not(windows))]
        match stream_once(url, &tx).await {
            Ok(()) => tracing::warn!(url = %url, "binance ws closed; reconnecting"),
            Err(e) => {
                let wait = reconnect_wait(&e);
                tracing::warn!(url = %url, error=%e, secs = wait.as_secs(), "binance ws dropped");
                if format!("{e:#}").contains("451") {
                    idx = (idx + 1) % urls.len();
                }
                tokio::time::sleep(wait).await;
                continue;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

fn reconnect_wait(err: &anyhow::Error) -> std::time::Duration {
    let msg = format!("{err:#}");
    if msg.contains("451") || msg.contains("Unavailable For Legal Reasons") {
        std::time::Duration::from_secs(2)
    } else {
        std::time::Duration::from_secs(1)
    }
}

#[cfg(not(windows))]
async fn stream_once(url: &str, tx: &EventTx) -> Result<()> {
    use futures_util::StreamExt;
    use tokio_tungstenite::{connect_async_with_config, tungstenite::Message};

    let (ws, _) = connect_async_with_config(url, None, true).await?;
    let (_sink, mut read) = ws.split();
    while let Some(msg) = read.next().await {
        let txt = match msg? {
            Message::Text(t) => t,
            Message::Ping(_) | Message::Pong(_) => continue,
            Message::Close(_) => break,
            _ => continue,
        };
        if let Some(ev) = parse_book_ticker(&txt) {
            let _ = tx.send(DatedEvent {
                recv_ns: now_ns(),
                event: ev,
            });
        }
    }
    Ok(())
}

/// Borrowed view of a bookTicker frame: no `Value` tree, no string copies.
/// `data` is the combined-stream envelope (`{"stream":…,"data":{…}}`).
#[derive(Deserialize)]
struct BookTicker<'a> {
    #[serde(default, borrow)]
    b: Option<&'a str>,
    #[serde(default, borrow)]
    a: Option<&'a str>,
    #[serde(default, rename = "T")]
    txn_ms: Option<u64>,
    #[serde(default, rename = "E")]
    event_ms: Option<u64>,
    #[serde(default, borrow)]
    data: Option<Box<BookTicker<'a>>>,
}

pub fn parse_book_ticker(txt: &str) -> Option<Event> {
    let frame: BookTicker = serde_json::from_str(txt).ok()?;
    let t = match frame.data {
        Some(inner) => *inner,
        None => frame,
    };
    let bid: f64 = t.b?.parse().ok()?;
    let ask: f64 = t.a?.parse().ok()?;
    Some(Event::BinanceBba {
        ts_exch_ms: t
            .txn_ms
            .or(t.event_ms)
            .unwrap_or_else(|| now_ns() / 1_000_000),
        bid,
        ask,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_usdm_book_ticker() {
        let s = r#"{"e":"bookTicker","u":1,"s":"BTCUSDT","b":"68825.1","B":"1","a":"68825.2","A":"1","T":1773072600123,"E":1773072600124}"#;
        match parse_book_ticker(s) {
            Some(Event::BinanceBba { bid, ask, ts_exch_ms }) => {
                assert!((bid - 68825.1).abs() < 1e-9);
                assert!((ask - 68825.2).abs() < 1e-9);
                assert_eq!(ts_exch_ms, 1773072600123);
            }
            _ => panic!("parse usdm"),
        }
    }

    #[test]
    fn parse_spot_book_ticker_without_t() {
        let s = r#"{"u":100703949398,"s":"BTCUSDT","b":"84016.62000000","B":"4.8","a":"84016.63000000","A":"3.2"}"#;
        match parse_book_ticker(s) {
            Some(Event::BinanceBba { bid, ask, ts_exch_ms }) => {
                assert!((bid - 84016.62).abs() < 1e-9);
                assert!((ask - 84016.63).abs() < 1e-9);
                assert!(ts_exch_ms > 0);
            }
            _ => panic!("spot bookTicker without T must parse"),
        }
    }

    #[test]
    fn parse_combined_stream_envelope() {
        let s = r#"{"stream":"btcusdt@bookTicker","data":{"e":"bookTicker","b":"1.5","B":"1","a":"1.6","A":"1","E":7}}"#;
        match parse_book_ticker(s) {
            Some(Event::BinanceBba { bid, ask, ts_exch_ms }) => {
                assert!((bid - 1.5).abs() < 1e-12);
                assert!((ask - 1.6).abs() < 1e-12);
                assert_eq!(ts_exch_ms, 7, "E when T is absent");
            }
            _ => panic!("combined stream must parse"),
        }
    }

    #[test]
    fn non_ticker_frames_are_ignored() {
        assert!(parse_book_ticker(r#"{"result":null,"id":1}"#).is_none());
        assert!(parse_book_ticker(r#"{"b":"1.0"}"#).is_none(), "ask required");
        assert!(parse_book_ticker("not json").is_none());
    }

    #[test]
    fn usdm_ws_is_primary() {
        let urls = ws_urls("BTCUSDT");
        assert!(urls[0].starts_with("wss://fstream.binance.com/ws/"));
        assert!(urls[0].ends_with("btcusdt@bookTicker"));
    }
}
