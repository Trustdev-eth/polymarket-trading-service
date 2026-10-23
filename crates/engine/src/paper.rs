//! Local CLOB stand-in for `PAPER=1`, driven by the live (or taped) book. Never touches the network.
//!
//! Execution follows Polymarket's crypto markets:
//! - An order reaches the venue `latency_ms` after submit (POST one way plus the book feed back).
//! - On arrival a crossing post-only is rejected, a non-marketable FAK is killed and a GTC rests.
//! - A marketable order is acked on arrival, then held for the market's taker delay. Held orders
//!   cannot be cancelled. On release the order takes displayed depth up to its limit; a FAK kills
//!   whatever is left.
//! - Liquidity this venue already took stays used until that level leaves the feed.

use std::collections::VecDeque;

use crate::math::compute_eff_fee;
use crate::types::{
    BookDepth, Command, ContractInfo, Event, IntentKind, Level, OrderIntent, Tif, TokenRef, DEPTH_LEVELS,
};

const EPS: f64 = 1e-9;
/// Below this many shares a level counts as empty.
const MIN_FILL: f64 = 1e-6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaperConfig {
    /// Submit → venue plus venue → our book feed: orders match the book we see this much later.
    pub latency_ms: u64,
    /// Overrides the market's `ContractInfo::taker_delay_ms` when set.
    pub taker_delay_ms: Option<u64>,
    /// Cap fills at displayed size from `Event::Depth`. Off = unlimited size at the BBO touch.
    pub depth: bool,
}

impl Default for PaperConfig {
    /// Instant, unlimited fills at the touch, for tapes that carry only the BBO.
    fn default() -> Self {
        Self {
            latency_ms: 0,
            taker_delay_ms: Some(0),
            depth: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PaperBook {
    pub yes_bid: f64,
    pub yes_ask: f64,
    pub depth: Option<BookDepth>,
}

impl PaperBook {
    pub fn no_ask(&self) -> f64 {
        if self.yes_bid > 0.0 {
            (1.0 - self.yes_bid).max(0.0)
        } else {
            0.0
        }
    }

    pub fn ready(&self) -> bool {
        self.yes_bid > 0.0 && self.yes_ask > 0.0 && self.yes_ask >= self.yes_bid
    }

    /// Offers for buying `token`, cheapest first, priced in that token.
    fn asks_for(&self, token: TokenRef, depth: bool) -> Asks {
        let mut a = Asks::default();
        if !depth {
            let touch = match token {
                TokenRef::Yes => self.yes_ask,
                TokenRef::No => self.no_ask(),
            };
            if self.ready() && touch > 0.0 {
                a.push(touch, f64::INFINITY);
            }
            return a;
        }
        let Some(d) = &self.depth else {
            return a;
        };
        match token {
            TokenRef::Yes => d.asks().for_each(|l| a.push(l.px, l.size)),
            TokenRef::No => d.bids().for_each(|l| a.push(round_px(1.0 - l.px), l.size)),
        }
        a
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Asks {
    lv: [Level; DEPTH_LEVELS],
    n: usize,
}

impl Asks {
    fn push(&mut self, px: f64, size: f64) {
        if self.n < DEPTH_LEVELS {
            self.lv[self.n] = Level { px, size };
            self.n += 1;
        }
    }

    fn iter(&self) -> impl Iterator<Item = &Level> {
        self.lv[..self.n].iter()
    }

    fn touch(&self) -> f64 {
        self.iter().next().map_or(0.0, |l| l.px)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaperOutcome {
    /// Filled in full.
    Filled,
    /// Part filled; the immediate remainder was killed.
    Partial,
    /// Not marketable when it reached the venue; killed without a fill.
    NotMarketable,
    /// Marketable on arrival, but nothing inside the limit was left at match time.
    NoFill,
    /// Post-only that would have crossed.
    Rejected,
}

impl PaperOutcome {
    pub fn name(self) -> &'static str {
        match self {
            PaperOutcome::Filled => "filled",
            PaperOutcome::Partial => "partial",
            PaperOutcome::NotMarketable => "not_marketable",
            PaperOutcome::NoFill => "no_fill",
            PaperOutcome::Rejected => "rejected",
        }
    }
}

/// How one order resolved. Emitted for immediate orders, rejects and delayed GTC matches.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaperExec {
    pub client_seq: u64,
    pub token: TokenRef,
    pub kind: IntentKind,
    pub limit_cents: i32,
    pub size: f64,
    pub filled: f64,
    /// Volume-weighted fill price; 0 when nothing filled.
    pub avg_px: f64,
    /// Cheapest offer for `token` at submit and at match time; 0 = none.
    pub touch_submit: f64,
    pub touch_match: f64,
    pub submit_ms: u64,
    pub match_ms: u64,
    pub outcome: PaperOutcome,
}

#[derive(Clone, Debug, Default)]
pub struct PaperLedger {
    pub markets_settled: u32,
    pub fills: u32,
    pub volume: f64,
    pub realized_pnl: f64,
    pub fees: f64,
    pub wins: u32,
    pub losses: u32,
    pub flats: u32,
    pub taker_orders: u32,
    pub taker_full: u32,
    pub taker_partial: u32,
    pub taker_missed: u32,
    pub taker_size: f64,
    pub taker_filled: f64,
}

impl PaperLedger {
    pub fn on_fill(&mut self, size: f64, fee: f64) {
        self.fills += 1;
        self.volume += size;
        self.fees += fee;
    }

    pub fn on_exec(&mut self, e: &PaperExec) {
        if e.kind != IntentKind::Taker {
            return;
        }
        self.taker_orders += 1;
        self.taker_size += e.size;
        self.taker_filled += e.filled;
        match e.outcome {
            PaperOutcome::Filled => self.taker_full += 1,
            PaperOutcome::Partial => self.taker_partial += 1,
            _ => self.taker_missed += 1,
        }
    }

    pub fn on_settle(&mut self, pnl: Option<f64>) {
        let Some(pnl) = pnl else {
            return;
        };
        self.markets_settled += 1;
        self.realized_pnl += pnl;
        if pnl > 1e-9 {
            self.wins += 1;
        } else if pnl < -1e-9 {
            self.losses += 1;
        } else {
            self.flats += 1;
        }
    }

    pub fn net_realized(&self) -> f64 {
        self.realized_pnl - self.fees
    }

    /// Share-weighted taker fill rate in [0, 1]; 0 before the first taker resolves.
    pub fn taker_fill_rate(&self) -> f64 {
        if self.taker_size > 0.0 {
            self.taker_filled / self.taker_size
        } else {
            0.0
        }
    }

    pub fn line(&self, mark_pnl: f64) -> String {
        format!(
            "PAPER SESSION markets={} fills={} vol={:.1} realized={:+.4} fees={:.4} net={:+.4} mark={:+.4} W/L/F={}/{}/{} takers={} fill_rate={:.1}% full/partial/miss={}/{}/{}",
            self.markets_settled,
            self.fills,
            self.volume,
            self.realized_pnl,
            self.fees,
            self.net_realized(),
            mark_pnl,
            self.wins,
            self.losses,
            self.flats,
            self.taker_orders,
            self.taker_fill_rate() * 100.0,
            self.taker_full,
            self.taker_partial,
            self.taker_missed,
        )
    }
}

#[derive(Clone, Copy, Debug)]
struct InFlight {
    intent: OrderIntent,
    submit_ms: u64,
    due_ms: u64,
    touch_submit: f64,
}

/// Shares this venue took at one price, which the feed does not know about.
#[derive(Clone, Copy, Debug)]
struct Used {
    token: TokenRef,
    key: i64,
    qty: f64,
}

#[derive(Clone, Debug, Default)]
pub struct PaperVenue {
    cfg: PaperConfig,
    fee_bps: u32,
    taker_delay_ms: u64,
    book: PaperBook,
    inbound: VecDeque<InFlight>,
    held: VecDeque<InFlight>,
    resting: Vec<OrderIntent>,
    used: Vec<Used>,
    reports: Vec<PaperExec>,
}

impl PaperVenue {
    pub fn new(cfg: PaperConfig) -> Self {
        let taker_delay_ms = cfg.taker_delay_ms.unwrap_or(0);
        Self {
            cfg,
            taker_delay_ms,
            ..Self::default()
        }
    }

    pub fn config(&self) -> PaperConfig {
        self.cfg
    }

    pub fn book(&self) -> &PaperBook {
        &self.book
    }

    pub fn taker_delay_ms(&self) -> u64 {
        self.taker_delay_ms
    }

    /// New market: everything in flight belongs to the closed window.
    pub fn on_contract(&mut self, info: &ContractInfo) {
        self.reset();
        self.fee_bps = info.fee_bps;
        self.taker_delay_ms = self.cfg.taker_delay_ms.unwrap_or(info.taker_delay_ms);
    }

    pub fn reset(&mut self) {
        self.inbound.clear();
        self.held.clear();
        self.resting.clear();
        self.used.clear();
        self.book = PaperBook::default();
    }

    /// Earliest time an in-flight order needs `advance`.
    pub fn next_due_ms(&self) -> Option<u64> {
        let a = self.inbound.front().map(|o| o.due_ms);
        let h = self.held.front().map(|o| o.due_ms);
        a.into_iter().chain(h).min()
    }

    pub fn take_reports(&mut self) -> Vec<PaperExec> {
        std::mem::take(&mut self.reports)
    }

    /// `Book` / `Depth` from the feed. Returns maker fills for resting orders the book crossed.
    pub fn on_market(&mut self, ev: &Event) -> Vec<Event> {
        match ev {
            Event::Book { yes_bid, yes_ask, .. } => self.set_book(*yes_bid, *yes_ask),
            Event::Depth(d) => self.set_depth(**d),
            _ => Vec::new(),
        }
    }

    pub fn set_book(&mut self, yes_bid: f64, yes_ask: f64) -> Vec<Event> {
        self.book.yes_bid = yes_bid;
        self.book.yes_ask = yes_ask;
        self.match_resting()
    }

    pub fn set_depth(&mut self, depth: BookDepth) -> Vec<Event> {
        self.book.depth = Some(depth);
        if !self.used.is_empty() {
            let book = self.book;
            let depth_on = self.cfg.depth;
            self.used.retain(|u| still_visible(&book.asks_for(u.token, depth_on), u.key));
        }
        self.match_resting()
    }

    /// Submits enter the latency queue; cancels act on resting orders. Returns what is due now.
    pub fn ingest_commands(&mut self, cmds: &[Command], now_ms: u64) -> Vec<Event> {
        let mut out = Vec::new();
        for c in cmds {
            match c {
                Command::Submit(i) => {
                    let touch_submit = self.asks(i.token).touch();
                    self.inbound.push_back(InFlight {
                        intent: *i,
                        submit_ms: now_ms,
                        due_ms: now_ms + self.cfg.latency_ms,
                        touch_submit,
                    });
                }
                Command::Cancel { client_seq, .. } => self.cancel(*client_seq, &mut out),
                Command::CancelAll { .. } | Command::Halt { .. } => self.cancel_all(),
            }
        }
        out.extend(self.advance(now_ms));
        out
    }

    /// Process arrivals and taker-delay releases due by `now_ms`, oldest first.
    pub fn advance(&mut self, now_ms: u64) -> Vec<Event> {
        let mut out = Vec::new();
        loop {
            let a = self.inbound.front().map(|o| o.due_ms).filter(|&t| t <= now_ms);
            let h = self.held.front().map(|o| o.due_ms).filter(|&t| t <= now_ms);
            match (a, h) {
                (Some(ta), Some(th)) if th < ta => self.release(&mut out),
                (Some(_), _) => self.arrive(&mut out),
                (None, Some(_)) => self.release(&mut out),
                (None, None) => break,
            }
        }
        out
    }

    fn asks(&self, token: TokenRef) -> Asks {
        self.book.asks_for(token, self.cfg.depth)
    }

    fn used(&self, token: TokenRef, key: i64) -> f64 {
        self.used
            .iter()
            .find(|u| u.token == token && u.key == key)
            .map_or(0.0, |u| u.qty)
    }

    fn consume(&mut self, token: TokenRef, key: i64, qty: f64) {
        match self.used.iter_mut().find(|u| u.token == token && u.key == key) {
            Some(u) => u.qty += qty,
            None => self.used.push(Used { token, key, qty }),
        }
    }

    /// Shares available inside `limit` after this venue's own takes.
    fn fillable(&self, token: TokenRef, limit: f64) -> f64 {
        self.asks(token)
            .iter()
            .take_while(|l| l.px <= limit + EPS)
            .map(|l| (l.size - self.used(token, px_key(l.px))).max(0.0))
            .sum()
    }

    fn cancel(&mut self, seq: u64, out: &mut Vec<Event>) {
        if self.held.iter().any(|o| o.intent.client_seq == seq) {
            out.push(Event::Reject { client_seq: seq });
            return;
        }
        self.inbound.retain(|o| o.intent.client_seq != seq);
        self.resting.retain(|i| i.client_seq != seq);
        out.push(ack_event(seq));
    }

    fn cancel_all(&mut self) {
        self.inbound.clear();
        self.resting.clear();
    }

    fn arrive(&mut self, out: &mut Vec<Event>) {
        let Some(o) = self.inbound.pop_front() else {
            return;
        };
        let i = o.intent;
        let marketable = self.fillable(i.token, limit_px(&i)) > MIN_FILL;
        if i.post_only {
            if marketable {
                out.push(Event::Reject { client_seq: i.client_seq });
                self.report(&o, 0.0, 0.0, o.due_ms, PaperOutcome::Rejected);
            } else {
                out.push(ack_event(i.client_seq));
                self.resting.push(i);
            }
            return;
        }
        out.push(ack_event(i.client_seq));
        if marketable {
            self.held.push_back(InFlight {
                due_ms: o.due_ms + self.taker_delay_ms,
                ..o
            });
        } else if i.tif.is_immediate() {
            self.report(&o, 0.0, 0.0, o.due_ms, PaperOutcome::NotMarketable);
        } else {
            self.resting.push(i);
        }
    }

    fn release(&mut self, out: &mut Vec<Event>) {
        let Some(o) = self.held.pop_front() else {
            return;
        };
        let mut i = o.intent;
        let limit = limit_px(&i);
        let all_or_none = i.tif == Tif::Fok && self.fillable(i.token, limit) + EPS < i.size;
        let (filled, cost) = if all_or_none { (0.0, 0.0) } else { self.take(&i, limit, out) };
        let outcome = if filled + EPS >= i.size {
            PaperOutcome::Filled
        } else if filled > MIN_FILL {
            PaperOutcome::Partial
        } else {
            PaperOutcome::NoFill
        };
        let avg_px = if filled > 0.0 { cost / filled } else { 0.0 };
        self.report(&o, filled, avg_px, o.due_ms, outcome);
        if !i.tif.is_immediate() && i.size - filled > MIN_FILL {
            i.size -= filled;
            self.resting.push(i);
        }
    }

    /// Walk the offers up to `limit`. Returns (shares, dollars).
    fn take(&mut self, i: &OrderIntent, limit: f64, out: &mut Vec<Event>) -> (f64, f64) {
        let mut left = i.size;
        let mut cost = 0.0;
        for l in self.asks(i.token).iter() {
            if left <= MIN_FILL || l.px > limit + EPS {
                break;
            }
            let key = px_key(l.px);
            let q = left.min(l.size - self.used(i.token, key));
            if q <= MIN_FILL {
                continue;
            }
            if self.cfg.depth {
                self.consume(i.token, key, q);
            }
            left -= q;
            cost += q * l.px;
            out.push(Event::Fill {
                client_seq: i.client_seq,
                token: i.token,
                px: l.px,
                size: q,
                fee: q * compute_eff_fee(l.px, self.fee_bps),
                is_taker: true,
            });
        }
        (i.size - left, cost)
    }

    /// A resting buy the offer side reached fills at its own price, as maker.
    fn match_resting(&mut self) -> Vec<Event> {
        let mut out = Vec::new();
        let mut k = 0;
        while k < self.resting.len() {
            let r = self.resting[k];
            let touch = self.asks(r.token).touch();
            let limit = limit_px(&r);
            if touch > 0.0 && touch <= limit + EPS {
                out.push(Event::Fill {
                    client_seq: r.client_seq,
                    token: r.token,
                    px: limit,
                    size: r.size,
                    fee: 0.0,
                    is_taker: false,
                });
                self.resting.swap_remove(k);
            } else {
                k += 1;
            }
        }
        out
    }

    fn report(&mut self, o: &InFlight, filled: f64, avg_px: f64, match_ms: u64, outcome: PaperOutcome) {
        let i = &o.intent;
        if !i.tif.is_immediate() && !matches!(outcome, PaperOutcome::Rejected) && filled <= MIN_FILL {
            return;
        }
        self.reports.push(PaperExec {
            client_seq: i.client_seq,
            token: i.token,
            kind: i.kind,
            limit_cents: i.px_ticks,
            size: i.size,
            filled,
            avg_px,
            touch_submit: o.touch_submit,
            touch_match: self.asks(i.token).touch(),
            submit_ms: o.submit_ms,
            match_ms,
            outcome,
        });
    }
}

fn limit_px(i: &OrderIntent) -> f64 {
    i.px_ticks as f64 / 100.0
}

fn px_key(px: f64) -> i64 {
    (px * 10_000.0).round() as i64
}

fn round_px(px: f64) -> f64 {
    (px * 10_000.0).round() / 10_000.0
}

/// Keep a used level while the feed still shows it, or while it sits beyond the visible window.
fn still_visible(asks: &Asks, key: i64) -> bool {
    if asks.iter().any(|l| px_key(l.px) == key) {
        return true;
    }
    asks.n == DEPTH_LEVELS && key > px_key(asks.lv[asks.n - 1].px)
}

fn ack_event(seq: u64) -> Event {
    let mut cloid = [0u8; 32];
    cloid[..8].copy_from_slice(&seq.to_le_bytes());
    cloid[31] = b'P';
    Event::Ack {
        client_seq: seq,
        cloid,
        ok: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{IntentKind, Side, Tif};

    fn buy(token: TokenRef, cents: i32, seq: u64, post_only: bool) -> OrderIntent {
        OrderIntent {
            token,
            side: Side::Buy,
            px_ticks: cents,
            size: 5.0,
            tif: Tif::Gtc,
            post_only,
            created_ms: 1,
            client_seq: seq,
            kind: IntentKind::Taker,
        }
    }

    fn fak(token: TokenRef, cents: i32, seq: u64) -> OrderIntent {
        OrderIntent {
            tif: Tif::Fak,
            ..buy(token, cents, seq, false)
        }
    }

    fn depth(bids: &[(f64, f64)], asks: &[(f64, f64)]) -> BookDepth {
        let mut d = BookDepth::default();
        for (k, &(px, size)) in bids.iter().enumerate() {
            d.bids[k] = Level { px, size };
        }
        for (k, &(px, size)) in asks.iter().enumerate() {
            d.asks[k] = Level { px, size };
        }
        d
    }

    fn realistic() -> PaperVenue {
        PaperVenue::new(PaperConfig {
            latency_ms: 25,
            taker_delay_ms: Some(150),
            depth: true,
        })
    }

    fn fills(evs: &[Event]) -> Vec<(f64, f64)> {
        evs.iter()
            .filter_map(|e| match e {
                Event::Fill { px, size, .. } => Some((*px, *size)),
                _ => None,
            })
            .collect()
    }

    fn submit(v: &mut PaperVenue, i: OrderIntent, now: u64) -> Vec<Event> {
        v.ingest_commands(&[Command::Submit(i)], now)
    }

    #[test]
    fn instant_taker_fills_at_ask_not_limit() {
        let mut v = PaperVenue::default();
        v.set_book(0.49, 0.51);
        let evs = submit(&mut v, buy(TokenRef::Yes, 80, 1, false), 0);
        assert_eq!(fills(&evs), vec![(0.51, 5.0)]);
        assert!(v.resting.is_empty());
    }

    #[test]
    fn gtc_miss_rests_then_fills_at_its_limit() {
        let mut v = PaperVenue::default();
        v.set_book(0.40, 0.70);
        assert!(fills(&submit(&mut v, buy(TokenRef::Yes, 50, 2, false), 0)).is_empty());
        assert_eq!(v.resting.len(), 1);
        let evs = v.set_book(0.40, 0.49);
        assert_eq!(fills(&evs), vec![(0.50, 5.0)], "maker fills at its own price");
        assert!(matches!(evs[0], Event::Fill { is_taker: false, fee, .. } if fee == 0.0));
        assert!(v.resting.is_empty());
    }

    #[test]
    fn fak_miss_does_not_rest() {
        let mut v = PaperVenue::default();
        v.set_book(0.40, 0.70);
        submit(&mut v, fak(TokenRef::Yes, 50, 5), 0);
        assert!(v.resting.is_empty());
        let r = v.take_reports();
        assert_eq!(r[0].outcome, PaperOutcome::NotMarketable);
    }

    #[test]
    fn post_only_rejects_cross() {
        let mut v = PaperVenue::default();
        v.set_book(0.49, 0.51);
        let evs = submit(&mut v, buy(TokenRef::Yes, 80, 3, true), 0);
        assert_eq!(evs, vec![Event::Reject { client_seq: 3 }]);
    }

    #[test]
    fn buy_no_fills_when_yes_bid_implies_no_ask() {
        let mut v = PaperVenue::default();
        v.set_book(0.60, 0.62);
        let evs = submit(&mut v, buy(TokenRef::No, 45, 4, false), 0);
        assert!(matches!(evs[1], Event::Fill { token: TokenRef::No, px, .. } if (px - 0.40).abs() < 1e-12));
    }

    #[test]
    fn taker_waits_latency_then_delay_before_matching() {
        let mut v = realistic();
        v.set_depth(depth(&[(0.49, 100.0)], &[(0.51, 100.0)]));
        assert!(submit(&mut v, fak(TokenRef::Yes, 51, 1), 1_000).is_empty());
        assert_eq!(v.next_due_ms(), Some(1_025));
        let ack = v.advance(1_025);
        assert!(matches!(ack[..], [Event::Ack { client_seq: 1, .. }]), "acked on arrival, not filled");
        assert_eq!(v.next_due_ms(), Some(1_175));
        assert!(v.advance(1_174).is_empty());
        assert_eq!(fills(&v.advance(1_175)), vec![(0.51, 5.0)]);
        let r = v.take_reports();
        assert_eq!((r[0].submit_ms, r[0].match_ms, r[0].outcome), (1_000, 1_175, PaperOutcome::Filled));
    }

    #[test]
    fn book_moving_during_delay_kills_the_fak() {
        let mut v = realistic();
        v.set_depth(depth(&[(0.49, 100.0)], &[(0.51, 100.0)]));
        submit(&mut v, fak(TokenRef::Yes, 51, 1), 0);
        v.advance(25);
        v.set_depth(depth(&[(0.52, 100.0)], &[(0.54, 100.0)]));
        assert!(fills(&v.advance(175)).is_empty());
        let r = v.take_reports();
        assert_eq!(r[0].outcome, PaperOutcome::NoFill);
        assert!((r[0].touch_submit - 0.51).abs() < 1e-12);
        assert!((r[0].touch_match - 0.54).abs() < 1e-12);
    }

    #[test]
    fn not_marketable_on_arrival_is_killed_without_delay() {
        let mut v = realistic();
        v.set_depth(depth(&[(0.49, 100.0)], &[(0.51, 100.0)]));
        submit(&mut v, fak(TokenRef::Yes, 51, 1), 0);
        v.set_depth(depth(&[(0.50, 100.0)], &[(0.53, 100.0)]));
        v.advance(25);
        assert_eq!(v.next_due_ms(), None);
        assert_eq!(v.take_reports()[0].outcome, PaperOutcome::NotMarketable);
    }

    #[test]
    fn thin_touch_partially_fills_and_kills_rest() {
        let mut v = realistic();
        v.set_depth(depth(&[(0.49, 100.0)], &[(0.51, 2.0), (0.52, 50.0)]));
        submit(&mut v, fak(TokenRef::Yes, 51, 1), 0);
        let evs = v.advance(175);
        assert_eq!(fills(&evs), vec![(0.51, 2.0)], "limit 51c never reaches the 52c level");
        let r = v.take_reports();
        assert_eq!(r[0].outcome, PaperOutcome::Partial);
        assert!((r[0].filled - 2.0).abs() < 1e-12);
    }

    #[test]
    fn limit_above_touch_walks_levels_with_vwap() {
        let mut v = realistic();
        v.set_depth(depth(&[(0.49, 100.0)], &[(0.51, 2.0), (0.52, 50.0)]));
        submit(&mut v, fak(TokenRef::Yes, 52, 1), 0);
        assert_eq!(fills(&v.advance(175)), vec![(0.51, 2.0), (0.52, 3.0)]);
        let r = v.take_reports()[0];
        assert!((r.avg_px - (2.0 * 0.51 + 3.0 * 0.52) / 5.0).abs() < 1e-12);
    }

    #[test]
    fn own_takes_stay_used_until_the_level_leaves() {
        let mut v = realistic();
        let book = depth(&[(0.49, 100.0)], &[(0.51, 7.0)]);
        v.set_depth(book);
        submit(&mut v, fak(TokenRef::Yes, 51, 1), 0);
        assert_eq!(fills(&v.advance(175)), vec![(0.51, 5.0)]);
        v.set_depth(book);
        submit(&mut v, fak(TokenRef::Yes, 51, 2), 200);
        assert_eq!(fills(&v.advance(375)), vec![(0.51, 2.0)], "feed still shows 7, 5 are ours");
        v.set_depth(depth(&[(0.49, 100.0)], &[(0.52, 9.0)]));
        v.set_depth(book);
        submit(&mut v, fak(TokenRef::Yes, 51, 3), 400);
        assert_eq!(fills(&v.advance(575)), vec![(0.51, 5.0)], "level refreshed after it left");
    }

    #[test]
    fn no_side_takes_mirrored_yes_bids() {
        let mut v = realistic();
        v.set_depth(depth(&[(0.60, 3.0), (0.59, 10.0)], &[(0.62, 100.0)]));
        submit(&mut v, fak(TokenRef::No, 41, 1), 0);
        assert_eq!(fills(&v.advance(175)), vec![(0.40, 3.0), (0.41, 2.0)]);
    }

    #[test]
    fn depth_mode_without_depth_never_fills() {
        let mut v = realistic();
        v.set_book(0.49, 0.51);
        submit(&mut v, fak(TokenRef::Yes, 60, 1), 0);
        v.advance(175);
        assert_eq!(v.take_reports()[0].outcome, PaperOutcome::NotMarketable);
    }

    #[test]
    fn held_order_cannot_be_cancelled() {
        let mut v = realistic();
        v.set_depth(depth(&[(0.49, 100.0)], &[(0.51, 100.0)]));
        submit(&mut v, buy(TokenRef::Yes, 51, 1, false), 0);
        v.advance(25);
        let evs = v.ingest_commands(&[Command::Cancel { client_seq: 1, reason: crate::types::CancelReason::Replace }], 30);
        assert_eq!(evs, vec![Event::Reject { client_seq: 1 }]);
        assert_eq!(fills(&v.advance(175)), vec![(0.51, 5.0)]);
    }

    #[test]
    fn contract_delay_applies_unless_overridden() {
        let info = ContractInfo {
            utc0_ms: 0,
            utc1_ms: 900_000,
            k_t: 1.0,
            fee_bps: 0,
            slug: String::new(),
            tick_size: 0.01,
            neg_risk: false,
            taker_delay_ms: 150,
        };
        let mut v = PaperVenue::new(PaperConfig {
            taker_delay_ms: None,
            ..PaperConfig::default()
        });
        v.on_contract(&info);
        assert_eq!(v.taker_delay_ms(), 150);
        let mut v = PaperVenue::new(PaperConfig::default());
        v.on_contract(&info);
        assert_eq!(v.taker_delay_ms(), 0);
    }

    #[test]
    fn ledger_tracks_fees_and_taker_fill_rate() {
        let mut led = PaperLedger::default();
        led.on_fill(5.0, 0.078125);
        led.on_settle(Some(2.5));
        assert!((led.net_realized() - (2.5 - 0.078125)).abs() < 1e-12);
        assert_eq!(led.wins, 1);
        let exec = PaperExec {
            client_seq: 1,
            token: TokenRef::Yes,
            kind: IntentKind::Taker,
            limit_cents: 51,
            size: 5.0,
            filled: 2.0,
            avg_px: 0.51,
            touch_submit: 0.51,
            touch_match: 0.51,
            submit_ms: 0,
            match_ms: 175,
            outcome: PaperOutcome::Partial,
        };
        led.on_exec(&exec);
        led.on_exec(&PaperExec { filled: 0.0, outcome: PaperOutcome::NoFill, ..exec });
        assert!((led.taker_fill_rate() - 0.2).abs() < 1e-12);
        assert!(led.line(0.0).contains("full/partial/miss=0/1/1"));
    }
}
