use std::collections::BTreeMap;

use anyhow::Result;
#[cfg(not(windows))]
use futures_util::{SinkExt, StreamExt};
use mm_engine::{BookDepth, Event, Level, DEPTH_LEVELS};
use serde_json::json;
#[cfg(not(windows))]
use tokio_tungstenite::{connect_async_with_config, tungstenite::Message};

use crate::{now_ns, DatedEvent, EventTx, POLY_MARKET_WS};

/// Subscribe to yes+no CLOB market WS. Emits `Book` for the Up/Yes token, plus `Depth` when
/// `depth` is set (paper venue sizing).
pub async fn run_market_ws(
    mut assets_rx: tokio::sync::watch::Receiver<(String, String)>,
    tx: EventTx,
    depth: bool,
) -> Result<()> {
    loop {
        // Mark the current ids as seen. A fresh/cloned watch receiver looks "changed"
        // until this runs; cloning it into select would abort the socket in a tight loop
        // and freeze the Yes/No + Signal charts.
        let (yes, no) = assets_rx.borrow_and_update().clone();
        if yes.is_empty() || yes == "0" {
            if assets_rx.changed().await.is_err() {
                return Ok(());
            }
            continue;
        }
        #[cfg(windows)]
        {
            let abort = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let abort_c = abort.clone();
            let yes_c = yes.clone();
            let no_c = no.clone();
            let tx_c = tx.clone();
            tokio::select! {
                res = tokio::task::spawn_blocking(move || stream_once_std(&yes_c, &no_c, &tx_c, depth, abort_c)) => {
                    match res {
                        Ok(Err(e)) => {
                            tracing::warn!(error=%e, "poly market ws dropped; reconnecting in 1s");
                            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                        }
                        Err(e) => tracing::warn!(error=%e, "poly market ws join"),
                        Ok(Ok(())) => {}
                    }
                }
                r = assets_rx.changed() => {
                    abort.store(true, std::sync::atomic::Ordering::Relaxed);
                    if r.is_err() {
                        return Ok(());
                    }
                    let (ny, nn) = assets_rx.borrow_and_update().clone();
                    if ny != yes || nn != no {
                        tracing::info!(yes_id=%ny, "poly market ws switching asset");
                    }
                }
            }
        }
        #[cfg(not(windows))]
        tokio::select! {
            res = stream_once(&yes, &no, &tx, depth) => {
                if let Err(e) = res {
                    tracing::warn!(error=%e, "poly market ws dropped; reconnecting in 1s");
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                }
            }
            r = assets_rx.changed() => {
                if r.is_err() {
                    return Ok(());
                }
                let (ny, nn) = assets_rx.borrow_and_update().clone();
                if ny != yes || nn != no {
                    tracing::info!(yes_id=%ny, "poly market ws switching asset");
                }
            }
        }
    }
}

fn subscription(yes_id: &str, no_id: &str) -> String {
    let mut ids = vec![yes_id.to_string()];
    if !no_id.is_empty() && no_id != "0" && no_id != yes_id {
        ids.push(no_id.to_string());
    }
    json!({
        "assets_ids": ids,
        "type": "market",
        "custom_feature_enabled": true,
    })
    .to_string()
}

fn forward(feed: &mut MarketFeed, txt: &str, tx: &EventTx) {
    let recv_ns = now_ns();
    for ev in feed.on_text(txt) {
        if let Event::Book { ts_ms, .. } = &ev {
            let lag = (recv_ns / 1_000_000) as i64 - *ts_ms as i64;
            if lag > 50 {
                tracing::debug!(lag_ms = lag, "poly book lag");
            }
        }
        let _ = tx.send(DatedEvent { recv_ns, event: ev });
    }
}

#[cfg(windows)]
fn stream_once_std(
    yes_id: &str,
    no_id: &str,
    tx: &EventTx,
    depth: bool,
    abort: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<()> {
    tracing::info!(yes_id, no_id, "poly market ws connecting");
    let mut feed = MarketFeed::new(yes_id, depth);
    crate::win_ws::connect_text_loop(
        POLY_MARKET_WS,
        Some(subscription(yes_id, no_id)),
        Some(abort),
        std::time::Duration::from_secs(10),
        |txt| forward(&mut feed, txt, tx),
    )
}

#[cfg(not(windows))]
async fn stream_once(yes_id: &str, no_id: &str, tx: &EventTx, depth: bool) -> Result<()> {
    tracing::info!(yes_id, no_id, "poly market ws connecting");
    let (ws, _) = connect_async_with_config(POLY_MARKET_WS, None, true).await?;
    let (mut sink, mut read) = ws.split();
    sink.send(Message::Text(subscription(yes_id, no_id).into())).await?;
    let mut feed = MarketFeed::new(yes_id, depth);
    let mut ping = tokio::time::interval(std::time::Duration::from_secs(10));
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            msg = read.next() => {
                let Some(msg) = msg else { break; };
                let txt = match msg? {
                    Message::Text(t) => t,
                    Message::Ping(p) => {
                        let _ = sink.send(Message::Pong(p)).await;
                        continue;
                    }
                    Message::Pong(_) => continue,
                    Message::Close(_) => break,
                    _ => continue,
                };
                forward(&mut feed, &txt, tx);
            }
            _ = ping.tick() => {
                sink.send(Message::Ping(Vec::new().into())).await?;
            }
        }
    }
    Ok(())
}

/// Parser for one market-WS connection. With depth on it also keeps the YES ladder and emits
/// `Event::Depth` only when the top `DEPTH_LEVELS` change (most deltas are far from the touch).
pub struct MarketFeed {
    yes_id: String,
    ladder: Option<Ladder>,
}

impl MarketFeed {
    pub fn new(yes_id: &str, depth: bool) -> Self {
        Self {
            yes_id: yes_id.to_string(),
            ladder: depth.then(Ladder::default),
        }
    }

    pub fn on_text(&mut self, txt: &str) -> Vec<Event> {
        let v: serde_json::Value = match serde_json::from_str(txt) {
            Ok(v) => v,
            Err(_) => return vec![],
        };
        let items: Vec<&serde_json::Value> = match v.as_array() {
            Some(a) => a.iter().collect(),
            None => vec![&v],
        };
        let mut out = Vec::with_capacity(items.len() + 1);
        let mut touched = false;
        let mut ts = 0;
        for item in items {
            if let Some(ev) = parse_one(item, &self.yes_id) {
                out.push(ev);
            }
            if let Some(l) = self.ladder.as_mut() {
                if l.apply(item, &self.yes_id) {
                    touched = true;
                    ts = ts.max(json_ts(item).unwrap_or(0));
                }
            }
        }
        if touched {
            let ladder = self.ladder.as_mut().expect("touched implies ladder");
            if let Some(d) = ladder.changed_top(ts) {
                out.push(Event::Depth(Box::new(d)));
            }
        }
        out
    }
}

/// YES price levels keyed in 1e-4 dollars (tick-agnostic).
#[derive(Default)]
struct Ladder {
    bids: BTreeMap<u32, f64>,
    asks: BTreeMap<u32, f64>,
    shown: Option<BookDepth>,
    /// Venue time of the newest snapshot / delta applied.
    last_ts: u64,
}

impl Ladder {
    /// Apply one WS item for the YES asset. True if the ladder may have changed.
    fn apply(&mut self, v: &serde_json::Value, yes_id: &str) -> bool {
        let ts = json_ts(v).unwrap_or(0);
        match v.get("event_type").and_then(|x| x.as_str()).unwrap_or("") {
            "book" if asset_id(v) == yes_id => {
                self.bids = levels(v.get("bids"));
                self.asks = levels(v.get("asks"));
                self.last_ts = self.last_ts.max(ts);
                true
            }
            "price_change" => {
                let Some(changes) = v.get("price_changes").and_then(|c| c.as_array()) else {
                    return false;
                };
                let mut any = false;
                for u in changes.iter().filter(|u| asset_id(u) == yes_id) {
                    let (Some(px), Some(size)) = (json_f64(u.get("price")), json_f64(u.get("size"))) else {
                        continue;
                    };
                    let side = match u.get("side").and_then(|s| s.as_str()) {
                        Some("BUY") => &mut self.bids,
                        Some("SELL") => &mut self.asks,
                        _ => continue,
                    };
                    set_level(side, px, size);
                    self.prune(u);
                    any = true;
                }
                if any {
                    self.last_ts = self.last_ts.max(ts);
                }
                any
            }
            // An older BBO must not delete levels a newer delta added.
            "best_bid_ask" if asset_id(v) == yes_id && ts >= self.last_ts => self.prune(v),
            _ => false,
        }
    }

    /// Drop levels a reported best bid / ask says are gone (guards against a missed delta).
    fn prune(&mut self, v: &serde_json::Value) -> bool {
        let mut any = false;
        if let Some(bb) = json_f64(v.get("best_bid")) {
            let k = px_key(bb);
            let before = self.bids.len();
            self.bids.retain(|&p, _| p <= k);
            any |= self.bids.len() != before;
        }
        if let Some(ba) = json_f64(v.get("best_ask")).filter(|&a| a > 0.0) {
            let k = px_key(ba);
            let before = self.asks.len();
            self.asks.retain(|&p, _| p >= k);
            any |= self.asks.len() != before;
        }
        any
    }

    fn changed_top(&mut self, ts_ms: u64) -> Option<BookDepth> {
        let mut d = BookDepth {
            ts_ms: if ts_ms > 0 { ts_ms } else { now_ns() / 1_000_000 },
            ..BookDepth::default()
        };
        fill_levels(&mut d.bids, self.bids.iter().rev());
        fill_levels(&mut d.asks, self.asks.iter());
        let same = self.shown.is_some_and(|s| s.bids == d.bids && s.asks == d.asks);
        if same {
            return None;
        }
        self.shown = Some(d);
        Some(d)
    }
}

fn fill_levels<'a>(dst: &mut [Level; DEPTH_LEVELS], src: impl Iterator<Item = (&'a u32, &'a f64)>) {
    for (slot, (&k, &size)) in dst.iter_mut().zip(src) {
        *slot = Level {
            px: k as f64 / 10_000.0,
            size,
        };
    }
}

fn px_key(px: f64) -> u32 {
    (px * 10_000.0).round().max(0.0) as u32
}

fn set_level(side: &mut BTreeMap<u32, f64>, px: f64, size: f64) {
    if size > 0.0 {
        side.insert(px_key(px), size);
    } else {
        side.remove(&px_key(px));
    }
}

fn levels(arr: Option<&serde_json::Value>) -> BTreeMap<u32, f64> {
    let mut out = BTreeMap::new();
    for lvl in arr.and_then(|a| a.as_array()).into_iter().flatten() {
        if let (Some(px), Some(size)) = (json_f64(lvl.get("price")), json_f64(lvl.get("size"))) {
            set_level(&mut out, px, size);
        }
    }
    out
}

fn asset_id(v: &serde_json::Value) -> &str {
    v.get("asset_id")
        .or_else(|| v.get("assetId"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
}

/// Stateless BBO parse of one WS frame (no depth).
pub fn parse_market_messages(txt: &str, yes_id: &str) -> Vec<Event> {
    MarketFeed::new(yes_id, false).on_text(txt)
}

pub fn parse_price_change(txt: &str, yes_id: &str) -> Option<Event> {
    parse_market_messages(txt, yes_id).into_iter().next()
}

fn parse_one(v: &serde_json::Value, yes_id: &str) -> Option<Event> {
    let et = v.get("event_type").and_then(|x| x.as_str()).unwrap_or("");
    match et {
        "best_bid_ask" => parse_bba(v, yes_id),
        "price_change" => parse_pc(v, yes_id),
        "book" => parse_book(v, yes_id),
        _ => parse_bba(v, yes_id).or_else(|| parse_pc(v, yes_id)),
    }
}

fn json_ts(v: &serde_json::Value) -> Option<u64> {
    v.get("timestamp")
        .and_then(|x| x.as_u64().or_else(|| x.as_str().and_then(|s| s.parse().ok())))
}

fn json_f64(v: Option<&serde_json::Value>) -> Option<f64> {
    let v = v?;
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

fn parse_bba(v: &serde_json::Value, yes_id: &str) -> Option<Event> {
    let id = asset_id(v);
    if !id.is_empty() && id != yes_id {
        return None;
    }
    let bid = json_f64(v.get("best_bid").or_else(|| v.get("bestBid")))?;
    let ask = json_f64(v.get("best_ask").or_else(|| v.get("bestAsk")))?;
    if bid <= 0.0 || ask <= 0.0 {
        return None;
    }
    Some(Event::Book {
        ts_ms: json_ts(v).unwrap_or_else(|| now_ns() / 1_000_000),
        yes_bid: bid,
        yes_ask: ask,
    })
}

fn parse_pc(v: &serde_json::Value, yes_id: &str) -> Option<Event> {
    let ts = json_ts(v).unwrap_or_else(|| now_ns() / 1_000_000);
    let changes = v.get("price_changes")?.as_array()?;
    for u in changes {
        if asset_id(u) != yes_id {
            continue;
        }
        let bid = json_f64(u.get("best_bid"))?;
        let ask = json_f64(u.get("best_ask"))?;
        return Some(Event::Book {
            ts_ms: ts,
            yes_bid: bid,
            yes_ask: ask,
        });
    }
    None
}

fn parse_book(v: &serde_json::Value, yes_id: &str) -> Option<Event> {
    let id = asset_id(v);
    if !id.is_empty() && id != yes_id {
        return None;
    }
    let best_bid = level_px(v.get("bids"), true)?;
    let best_ask = level_px(v.get("asks"), false)?;
    Some(Event::Book {
        ts_ms: json_ts(v).unwrap_or_else(|| now_ns() / 1_000_000),
        yes_bid: best_bid,
        yes_ask: best_ask,
    })
}

fn level_px(arr: Option<&serde_json::Value>, is_bid: bool) -> Option<f64> {
    let a = arr?.as_array()?;
    let mut best: Option<f64> = None;
    for lvl in a {
        let px = json_f64(lvl.get("price")).or_else(|| {
            lvl.as_array()
                .and_then(|v| v.first())
                .and_then(|x| json_f64(Some(x)))
        })?;
        best = Some(match best {
            None => px,
            Some(b) if is_bid => b.max(px),
            Some(b) => b.min(px),
        });
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    const SNAPSHOT: &str = r#"[{"event_type":"book","asset_id":"YES","timestamp":"1000","bids":[{"price":"0.22","size":"2619.2"},{"price":"0.23","size":"730.74"},{"price":"0.24","size":"548"}],"asks":[{"price":"0.27","size":"2044"},{"price":"0.26","size":"317"},{"price":"0.25","size":"166.5"}]},{"event_type":"book","asset_id":"NO","timestamp":"1000","bids":[{"price":"0.75","size":"166.5"}],"asks":[{"price":"0.76","size":"548"}]}]"#;

    fn depth_of(evs: &[Event]) -> Option<BookDepth> {
        evs.iter().find_map(|e| match e {
            Event::Depth(d) => Some(**d),
            _ => None,
        })
    }

    #[test]
    fn parse_pc() {
        let s = r#"{"event_type":"price_change","timestamp":"1773072600500","price_changes":[{"asset_id":"YES","best_bid":"0.48","best_ask":"0.52"}]}"#;
        match parse_price_change(s, "YES") {
            Some(Event::Book { yes_bid, yes_ask, .. }) => {
                assert!((yes_bid - 0.48).abs() < 1e-9);
                assert!((yes_ask - 0.52).abs() < 1e-9);
            }
            _ => panic!("parse"),
        }
    }

    #[test]
    fn parse_best_bid_ask() {
        let s = r#"{"event_type":"best_bid_ask","asset_id":"YES","best_bid":"0.49","best_ask":"0.51","timestamp":1773072600500}"#;
        match parse_price_change(s, "YES") {
            Some(Event::Book { yes_bid, yes_ask, .. }) => {
                assert!((yes_bid - 0.49).abs() < 1e-9);
                assert!((yes_ask - 0.51).abs() < 1e-9);
            }
            _ => panic!("bba"),
        }
    }

    #[test]
    fn snapshot_builds_yes_depth_best_first() {
        let mut f = MarketFeed::new("YES", true);
        let evs = f.on_text(SNAPSHOT);
        assert!(matches!(evs[0], Event::Book { yes_bid, yes_ask, .. } if yes_bid == 0.24 && yes_ask == 0.25));
        let d = depth_of(&evs).expect("depth");
        assert_eq!(d.ts_ms, 1000);
        assert_eq!(d.bids[0], Level { px: 0.24, size: 548.0 });
        assert_eq!(d.bids[2], Level { px: 0.22, size: 2619.2 });
        assert_eq!(d.asks[0], Level { px: 0.25, size: 166.5 });
        assert_eq!(d.asks().count(), 3);
    }

    #[test]
    fn price_change_updates_and_removes_levels() {
        let mut f = MarketFeed::new("YES", true);
        f.on_text(SNAPSHOT);
        let pc = r#"{"event_type":"price_change","timestamp":"1001","price_changes":[{"asset_id":"YES","price":"0.25","size":"0","side":"SELL","best_bid":"0.24","best_ask":"0.26"},{"asset_id":"NO","price":"0.75","size":"0","side":"BUY","best_bid":"0.74","best_ask":"0.76"},{"asset_id":"YES","price":"0.24","size":"600","side":"BUY","best_bid":"0.24","best_ask":"0.26"}]}"#;
        let d = depth_of(&f.on_text(pc)).expect("depth");
        assert_eq!(d.asks[0], Level { px: 0.26, size: 317.0 });
        assert_eq!(d.bids[0], Level { px: 0.24, size: 600.0 });
    }

    #[test]
    fn deep_changes_do_not_emit_depth() {
        let mut f = MarketFeed::new("YES", true);
        f.on_text(SNAPSHOT);
        let mut deep = String::from(r#"{"event_type":"price_change","price_changes":["#);
        for k in 0..5 {
            if k > 0 {
                deep.push(',');
            }
            deep.push_str(&format!(r#"{{"asset_id":"YES","price":"0.{}","size":"10","side":"SELL","best_bid":"0.24","best_ask":"0.25"}}"#, 30 + k));
        }
        deep.push_str("]}");
        let evs = f.on_text(&deep);
        assert!(depth_of(&evs).is_some(), "levels 4-5 of the window filled");
        let far = r#"{"event_type":"price_change","price_changes":[{"asset_id":"YES","price":"0.90","size":"10","side":"SELL","best_bid":"0.24","best_ask":"0.25"}]}"#;
        assert!(depth_of(&f.on_text(far)).is_none());
    }

    #[test]
    fn best_bid_ask_prunes_stale_levels() {
        let mut f = MarketFeed::new("YES", true);
        f.on_text(SNAPSHOT);
        let bba = r#"{"event_type":"best_bid_ask","asset_id":"YES","best_bid":"0.23","best_ask":"0.26","timestamp":"1002"}"#;
        let d = depth_of(&f.on_text(bba)).expect("depth");
        assert_eq!(d.bids[0].px, 0.23);
        assert_eq!(d.asks[0].px, 0.26);
    }

    #[test]
    fn stale_best_bid_ask_does_not_prune_newer_levels() {
        let mut f = MarketFeed::new("YES", true);
        f.on_text(SNAPSHOT);
        let pc = r#"{"event_type":"price_change","timestamp":"2000","price_changes":[{"asset_id":"YES","price":"0.245","size":"40","side":"BUY","best_bid":"0.245","best_ask":"0.25"}]}"#;
        f.on_text(pc);
        let stale = r#"{"event_type":"best_bid_ask","asset_id":"YES","best_bid":"0.24","best_ask":"0.25","timestamp":"1999"}"#;
        assert!(depth_of(&f.on_text(stale)).is_none());
        let fresh = r#"{"event_type":"best_bid_ask","asset_id":"YES","best_bid":"0.24","best_ask":"0.25","timestamp":"2001"}"#;
        assert_eq!(depth_of(&f.on_text(fresh)).expect("pruned").bids[0].px, 0.24);
    }

    #[test]
    fn no_depth_events_without_depth_mode() {
        let mut f = MarketFeed::new("YES", false);
        assert!(depth_of(&f.on_text(SNAPSHOT)).is_none());
    }
}
