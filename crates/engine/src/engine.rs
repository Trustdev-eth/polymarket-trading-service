//! Event ingestion: Binance Ŝ, Chainlink / TWAP references, CLOB book → GTR bands.

use crate::bands::{Band, Bands};
use crate::bins::Bins;
use crate::config::EngineConfig;
use crate::math::{
    ewma, nd2_scaled, shift_oracle_price, sigest_from_ewma, DELTA_EWMA_ALPHA, FAST_VAR_ALPHA,
    SHIFT_ALPHA, SLOW_VAR_ALPHA,
};
use crate::oms::Oms;
use crate::paper::{PaperLedger, PaperVenue};
use crate::types::{AllowTrade, CancelReason, Command, ContractInfo, Event, HaltReason, Position, TakeSkip};

#[derive(Clone, Debug, Default)]
pub struct State {
    pub info: Option<ContractInfo>,
    pub allow: AllowTrade,
    pub halt_until_ms: u64,

    pub bba_ts_ms: u64,
    /// True when `bba_ts_ms` last came from Binance (not Chainlink fallback).
    pub bba_from_binance: bool,
    pub fast_mid: f64,
    pub s_hat: f64,
    pub last_s_hat: f64,
    pub shift_ewma: f64,
    pub delta_ewma: f64,
    pub last_mid_for_delta: f64,
    pub var_fast: f64,
    pub var_slow: f64,
    pub var_countdown: u32,
    pub last_mid_for_var: f64,

    pub chainlink: f64,
    pub chainlink_ts_ms: u64,
    pub twap: f64,
    /// Last `crypto_prices_twap_sixty` print inside the lock-in window (already a 60s TWAP).
    pub settle_twap: f64,
    pub settle_close: f64,

    pub yes_bid: f64,
    pub yes_ask: f64,
    pub book_mid: f64,
    pub book_ts_ms: u64,

    pub quote_ts_ms: u64,
    pub quote_bid: f64,
    pub quote_ask: f64,
    pub fak_long: f64,
    pub fak_short: f64,
    pub ds_s_long: f64,
    pub ds_s_short: f64,

    pub last_take_p: i32,
    pub last_take_ms: u64,
    /// Latest signal past `taker_ds_s` that the fee / fair-ask gate refused.
    pub skip: Option<TakeSkip>,
    pub last_hedge_ms: u64,
    pub position: Position,
}

impl State {
    pub fn tau_s(&self, now_ms: u64) -> f64 {
        match &self.info {
            Some(c) if c.utc1_ms > now_ms => (c.utc1_ms - now_ms) as f64 / 1000.0,
            _ => 0.0,
        }
    }

    pub fn k_t(&self) -> f64 {
        self.info.as_ref().map(|i| i.k_t).unwrap_or(0.0)
    }

    pub fn locked_weight(&self, now_ms: u64, window_s: f64) -> f64 {
        ((window_s - self.tau_s(now_ms)) / window_s).clamp(0.0, 1.0)
    }

    pub fn locked_s(&self) -> f64 {
        if self.settle_twap > 0.0 {
            self.settle_twap
        } else {
            self.twap
        }
    }
}

/// Deterministic engine. Same events + clock ⇒ same commands. No IO.
pub struct Engine {
    pub cfg: EngineConfig,
    pub state: State,
    pub fast_bins: Bins,
    pub oracle_bins: Bins,
    pub oms: Oms,
    pub bands: Bands,
}

impl Engine {
    pub fn new(cfg: EngineConfig) -> Self {
        let state = State {
            var_fast: crate::math::INIT_VAR,
            var_slow: crate::math::INIT_VAR,
            var_countdown: 1,
            ..State::default()
        };
        Self {
            fast_bins: Bins::new(cfg.bucket_ms, cfg.fast_bin_step_ms),
            oracle_bins: Bins::new(cfg.bucket_ms, cfg.oracle_bin_step_ms),
            cfg,
            state,
            oms: Oms::default(),
            bands: Bands::default(),
        }
    }

    pub fn on_event(&mut self, event: Event, now_ms: u64) -> Vec<Command> {
        let mut out = Vec::with_capacity(4);
        self.on_event_into(event, now_ms, &mut out);
        out
    }

    /// Hot path: `out` is cleared and reused.
    pub fn on_event_into(&mut self, event: Event, now_ms: u64, out: &mut Vec<Command>) {
        out.clear();
        match event {
            Event::LoadContract(info) => self.on_contract(info, out),
            Event::SeedPosition(p) => self.state.position = p,
            Event::BinanceBba { ts_exch_ms, bid, ask } => {
                self.on_bba(ts_exch_ms, bid, ask, now_ms, out, true)
            }
            Event::Twap { ts_ms, value } => self.on_twap(ts_ms, value, now_ms, out),
            Event::Chainlink { ts_ms, value } => self.on_chainlink(ts_ms, value, now_ms, out),
            Event::Book { ts_ms, yes_bid, yes_ask } => self.on_book(ts_ms, yes_bid, yes_ask, now_ms, out),
            Event::Depth(_) => {}
            Event::Ack { client_seq, ok, .. } => {
                if ok {
                    self.oms.on_ack(client_seq, now_ms, out);
                } else {
                    self.oms.on_reject(client_seq);
                }
            }
            Event::Reject { client_seq } => self.oms.on_reject(client_seq),
            Event::Fill { client_seq, token, px, size, fee, .. } => {
                if client_seq != 0 && size > 0.0 {
                    self.state.position.add(token, px, size, fee);
                    self.oms.on_fill(client_seq, size);
                }
            }
            Event::InvariantTick => self.on_invariant(now_ms, out),
            Event::Settle { close } => {
                if close > 0.0 {
                    self.state.settle_close = close;
                }
            }
        }
    }

    fn on_contract(&mut self, info: ContractInfo, out: &mut Vec<Command>) {
        let same_bucket = self.state.info.as_ref().is_some_and(|p| p.utc0_ms == info.utc0_ms);
        if !same_bucket {
            if self.state.info.is_some() {
                out.push(Command::CancelAll { reason: CancelReason::Halt });
            }
            self.reset_market(info.utc0_ms);
        }
        self.state.info = Some(info);
    }

    /// Per-market reset. Ŝ shift and the last references carry over.
    fn reset_market(&mut self, utc0: u64) {
        self.fast_bins.roll(utc0);
        self.oracle_bins.roll(utc0);
        self.oms.reset();
        self.bands.clear();
        let s = &mut self.state;
        s.allow = AllowTrade::WaitingBook;
        s.var_fast = crate::math::INIT_VAR;
        s.var_slow = crate::math::INIT_VAR;
        s.var_countdown = 1;
        s.last_mid_for_var = 0.0;
        s.last_mid_for_delta = 0.0;
        s.delta_ewma = 0.0;
        s.settle_twap = 0.0;
        s.settle_close = 0.0;
        s.yes_bid = 0.0;
        s.yes_ask = 0.0;
        s.book_mid = 0.0;
        s.book_ts_ms = 0;
        s.quote_ts_ms = 0;
        s.fak_long = 0.0;
        s.fak_short = 0.0;
        s.ds_s_long = 0.0;
        s.ds_s_short = 0.0;
        s.last_take_p = 0;
        s.last_take_ms = 0;
        s.last_hedge_ms = 0;
        s.position = Position::default();
    }

    fn on_bba(&mut self, ts: u64, bid: f64, ask: f64, now_ms: u64, out: &mut Vec<Command>, from_binance: bool) {
        if bid <= 0.0 || ask < bid {
            return;
        }
        self.apply_oracle_mid(ts, 0.5 * (bid + ask), now_ms, out, from_binance);
    }

    fn apply_oracle_mid(&mut self, ts: u64, mid: f64, now_ms: u64, out: &mut Vec<Command>, from_binance: bool) {
        if mid <= 0.0 {
            return;
        }
        let bucket = ts / self.cfg.bucket_ms * self.cfg.bucket_ms;
        if self.fast_bins.bin_start() != bucket {
            self.fast_bins.roll(bucket);
            self.oracle_bins.roll(bucket);
            self.state.last_mid_for_delta = 0.0;
        }

        let s = &mut self.state;
        s.bba_ts_ms = ts;
        s.bba_from_binance = from_binance;
        if s.last_mid_for_delta > 0.0 {
            s.delta_ewma = ewma(DELTA_EWMA_ALPHA, mid - s.last_mid_for_delta, s.delta_ewma);
        }
        s.last_mid_for_delta = mid;

        s.var_countdown = s.var_countdown.saturating_sub(1);
        if s.var_countdown == 0 {
            if s.last_mid_for_var > 0.0 {
                let lr = (mid / s.last_mid_for_var).ln();
                s.var_fast = ewma(FAST_VAR_ALPHA, lr * lr, s.var_fast);
                s.var_slow = ewma(SLOW_VAR_ALPHA, lr * lr, s.var_slow);
            }
            s.last_mid_for_var = mid;
            s.var_countdown = self.cfg.var_sample_ticks.max(1);
        }

        let s_hat = shift_oracle_price(mid, s.shift_ewma, s.delta_ewma);
        let jumped = s.last_s_hat > 0.0 && ((s_hat - s.last_s_hat) / s.last_s_hat).abs() > self.cfg.jump_frac;
        s.s_hat = s_hat;
        s.last_s_hat = s_hat;
        self.fast_bins.write(ts, mid);
        self.oracle_bins.write(ts, s_hat);
        if jumped {
            self.oms.cancel_all(CancelReason::Jump, now_ms, out);
            self.state.last_take_p = 0;
        }

        if mid != self.state.fast_mid {
            self.state.fast_mid = mid;
        }
        // CLOB only sends on BBO change. Keep a *fresh* last book in GTR so Ŝ can move the signal.
        self.keep_gtr_alive(now_ms, out);
        self.policy(now_ms, out);
    }

    fn update_shift(&mut self, ts_ms: u64, reference: f64) {
        if let Some(bba) = self.fast_bins.get_at_ts(ts_ms) {
            let inst = bba - reference;
            self.state.shift_ewma = if self.state.shift_ewma == 0.0 {
                inst
            } else {
                ewma(SHIFT_ALPHA, inst, self.state.shift_ewma)
            };
        }
    }

    fn on_twap(&mut self, ts_ms: u64, value: f64, now_ms: u64, out: &mut Vec<Command>) {
        if value <= 0.0 {
            return;
        }
        self.state.twap = value;
        let Some(info) = self.state.info.as_mut() else {
            return;
        };
        if info.k_t <= 0.0 && ts_ms + 2_000 >= info.utc0_ms && ts_ms <= info.utc0_ms + 5_000 {
            info.k_t = value;
        }
        if ts_ms / self.cfg.bucket_ms * self.cfg.bucket_ms != info.utc0_ms {
            self.oms.cancel_all(CancelReason::Halt, ts_ms, out);
        }
        if self.state.chainlink <= 0.0 {
            self.update_shift(ts_ms, value);
        }
        if let Some(info) = &self.state.info {
            if info.utc1_ms.saturating_sub(now_ms.max(ts_ms)) as f64 <= self.cfg.twap_window_s * 1000.0 {
                self.state.settle_twap = value;
            }
        }
    }

    fn on_chainlink(&mut self, ts_ms: u64, value: f64, now_ms: u64, out: &mut Vec<Command>) {
        if value <= 0.0 {
            return;
        }
        self.state.chainlink = value;
        self.state.chainlink_ts_ms = ts_ms;
        self.update_shift(ts_ms, value);
        if !self.binance_bba_fresh(now_ms) {
            self.apply_oracle_mid(ts_ms, value, now_ms, out, false);
        }
    }

    fn binance_bba_fresh(&self, now_ms: u64) -> bool {
        self.state.bba_from_binance
            && self.state.bba_ts_ms > 0
            && now_ms.saturating_sub(self.state.bba_ts_ms) <= self.cfg.bba_fresh_ms
    }

    /// Binance bookTicker within 1s, or Chainlink-driven Ŝ within `bba_stale_ms`.
    pub(crate) fn oracle_fresh_for_policy(&self, now_ms: u64) -> bool {
        if self.binance_bba_fresh(now_ms) {
            return true;
        }
        !self.state.bba_from_binance && self.chainlink_fresh(now_ms)
    }

    pub(crate) fn book_fresh(&self, now_ms: u64) -> bool {
        self.state.book_ts_ms > 0
            && now_ms.saturating_sub(self.state.book_ts_ms) <= self.cfg.book_stale_ms
    }

    fn chainlink_fresh(&self, now_ms: u64) -> bool {
        self.state.chainlink > 0.0
            && self.state.chainlink_ts_ms > 0
            && now_ms.saturating_sub(self.state.chainlink_ts_ms) <= self.cfg.bba_stale_ms
    }

    /// Underlying for nd2: fresh Chainlink, else Ŝ, else TWAP.
    pub(crate) fn model_s(&self, now_ms: u64) -> f64 {
        if self.chainlink_fresh(now_ms) {
            self.state.chainlink
        } else if self.state.s_hat > 0.0 {
            self.state.s_hat
        } else {
            self.state.twap
        }
    }

    pub(crate) fn sigma(&self, tau: f64) -> f64 {
        sigest_from_ewma(
            self.state.var_fast,
            self.state.var_slow,
            tau,
            self.cfg.vol_tau_min,
            self.cfg.vol_tau_max,
        )
    }

    fn on_book(&mut self, book_ts: u64, yes_bid: f64, yes_ask: f64, now_ms: u64, out: &mut Vec<Command>) {
        if yes_bid <= 0.0 || yes_ask <= 0.0 || yes_ask < yes_bid {
            return;
        }
        let unchanged = self.state.book_ts_ms > 0 && yes_bid == self.state.yes_bid && yes_ask == self.state.yes_ask;
        self.state.yes_bid = yes_bid;
        self.state.yes_ask = yes_ask;
        match self.state.allow {
            AllowTrade::WaitingBook => {
                self.state.allow = AllowTrade::Cooldown(now_ms + self.cfg.book_cooldown_ms);
            }
            AllowTrade::Cooldown(until) if now_ms >= until => self.state.allow = AllowTrade::Ready,
            _ => {}
        }
        // A frozen CLOB (same 0.999/1.00 all window) must still mint the first band.
        if unchanged && self.bands.newest().is_some() {
            self.bands.extend_newest(book_ts + self.cfg.gtr_ms);
            self.state.book_ts_ms = book_ts;
            return self.policy(now_ms, out);
        }
        let mid = 0.5 * (yes_bid + yes_ask);
        self.state.book_mid = mid;
        self.state.book_ts_ms = book_ts;

        if self.state.tau_s(now_ms) <= self.cfg.no_hedge_s {
            self.oms.cancel_all(CancelReason::NoQuote, now_ms, out);
            return;
        }
        if !self.try_push_band(book_ts, now_ms) && self.bands.newest().is_none() {
            return;
        }
        self.policy(now_ms, out);
    }

    /// Quiet CLOB: last BBO is still the live quote. A silent book older than
    /// `book_stale_ms` is treated as disconnected — drop GTR instead of quoting it.
    fn keep_gtr_alive(&mut self, now_ms: u64, out: &mut Vec<Command>) {
        if self.state.tau_s(now_ms) <= self.cfg.no_hedge_s {
            return;
        }
        if self.state.yes_bid <= 0.0 || self.state.yes_ask <= 0.0 || self.state.yes_ask < self.state.yes_bid {
            return;
        }
        if !self.book_fresh(now_ms) {
            let had = self.bands.newest().is_some()
                || self.oms.has_kind(crate::types::IntentKind::Taker)
                || self.oms.has_kind(crate::types::IntentKind::Hedge);
            self.bands.clear();
            if had {
                self.oms.cancel_all(CancelReason::StaleBand, now_ms, out);
            }
            self.state.allow = AllowTrade::WaitingBook;
            return;
        }
        if self.bands.newest().is_some() {
            self.bands.extend_newest(now_ms + self.cfg.gtr_ms);
            return;
        }
        let ts = if self.state.book_ts_ms > 0 {
            self.state.book_ts_ms
        } else {
            now_ms
        };
        let _ = self.try_push_band(ts, now_ms);
    }

    fn try_push_band(&mut self, book_ts: u64, now_ms: u64) -> bool {
        let tau = self.state.tau_s(now_ms);
        let k_t = self.state.k_t();
        let mid = self.state.book_mid;
        if mid <= 0.0 {
            return false;
        }
        let ingest = self.cfg.book_ingest_ms();
        let mut ts = book_ts;
        let mut ts_min = ts.saturating_sub(ingest + self.cfg.taker_bump_ms + self.cfg.book_extra_ms);
        let mut ts_max = ts.saturating_sub(self.cfg.fiber_trip_ms + self.cfg.order_serve_ms);
        let got = (
            self.oracle_bins.range_minmax(ts_min, ts_max),
            self.oracle_bins.get_at_ts(ts.saturating_sub(ingest)),
        );
        let (ref_min, ref_max, likely_ref) = match got {
            (Some((lo, hi, _)), Some(r)) => (lo, hi, r),
            _ if ts != now_ms => {
                ts = now_ms;
                ts_min = ts.saturating_sub(ingest + self.cfg.taker_bump_ms + self.cfg.book_extra_ms);
                ts_max = ts.saturating_sub(self.cfg.fiber_trip_ms + self.cfg.order_serve_ms);
                match (
                    self.oracle_bins.range_minmax(ts_min, ts_max),
                    self.oracle_bins.get_at_ts(ts.saturating_sub(ingest)),
                ) {
                    (Some((lo, hi, _)), Some(r)) => (lo, hi, r),
                    _ => return false,
                }
            }
            _ => return false,
        };

        let sigma = self.sigma(tau);
        let s_model = self.model_s(now_ms);
        let (nd2_min, nd2_max) = if s_model > 0.0 && self.chainlink_fresh(now_ms) {
            let n = nd2_scaled(s_model, k_t, tau, sigma);
            (n, n)
        } else {
            let n = [ref_min, 0.5 * (ref_min + ref_max), ref_max].map(|r| nd2_scaled(r, k_t, tau, sigma));
            (
                n.iter().copied().fold(f64::MAX, f64::min),
                n.iter().copied().fold(0.0, f64::max),
            )
        };
        if nd2_max <= 0.0 {
            return false;
        }
        self.bands.push(Band {
            mid,
            ref_min,
            ref_max,
            likely_ref,
            nd2_scaled_min: nd2_min,
            nd2_scaled_max: nd2_max,
            expires_ms: now_ms.max(ts) + self.cfg.gtr_ms,
        });
        true
    }

    fn on_invariant(&mut self, now_ms: u64, out: &mut Vec<Command>) {
        self.oms.expire(now_ms, self.cfg.order_timeout_ms);
        let oracle_ok = self.binance_bba_fresh(now_ms) || self.chainlink_fresh(now_ms);
        if self.state.bba_ts_ms > 0 && !oracle_ok && now_ms.saturating_sub(self.state.bba_ts_ms) > self.cfg.bba_stale_ms
        {
            let until = now_ms + self.cfg.halt_ms;
            self.state.halt_until_ms = until;
            self.state.allow = AllowTrade::Cooldown(until);
            self.oms.cancel_all(CancelReason::Halt, now_ms, out);
            out.push(Command::Halt {
                until_ms: until,
                reason: HaltReason::BbaStale,
            });
        }
        if self.state.info.is_none() {
            self.state.allow = AllowTrade::WaitingBook;
            out.push(Command::Halt {
                until_ms: now_ms + self.cfg.bucket_ms,
                reason: HaltReason::BookMissing,
            });
        }
        self.keep_gtr_alive(now_ms, out);
        self.policy(now_ms, out);
    }

    /// Paper: hand already-emitted `cmds` to the venue. Returns follow-on cmds and exec events.
    pub fn paper_follow(
        &mut self,
        venue: &mut PaperVenue,
        ledger: &mut PaperLedger,
        cmds: &[Command],
        now_ms: u64,
    ) -> (Vec<Command>, Vec<Event>) {
        if cmds.is_empty() {
            return (Vec::new(), Vec::new());
        }
        let evs = venue.ingest_commands(cmds, now_ms);
        self.paper_drain(venue, ledger, evs, now_ms)
    }

    /// Paper: arrivals and taker-delay releases due by `now_ms`.
    pub fn paper_advance(
        &mut self,
        venue: &mut PaperVenue,
        ledger: &mut PaperLedger,
        now_ms: u64,
    ) -> (Vec<Command>, Vec<Event>) {
        let evs = venue.advance(now_ms);
        self.paper_drain(venue, ledger, evs, now_ms)
    }

    /// Paper: `Book` / `Depth` update; resting orders the book reached fill as maker.
    pub fn paper_on_market(
        &mut self,
        venue: &mut PaperVenue,
        ledger: &mut PaperLedger,
        ev: &Event,
        now_ms: u64,
    ) -> (Vec<Command>, Vec<Event>) {
        let evs = venue.on_market(ev);
        self.paper_drain(venue, ledger, evs, now_ms)
    }

    /// Feed venue events to the engine and its answers back to the venue until quiet.
    fn paper_drain(
        &mut self,
        venue: &mut PaperVenue,
        ledger: &mut PaperLedger,
        mut evs: Vec<Event>,
        now_ms: u64,
    ) -> (Vec<Command>, Vec<Event>) {
        let mut sent = Vec::new();
        let mut exec = Vec::new();
        let mut cmds = Vec::new();
        for _ in 0..8 {
            if evs.is_empty() {
                break;
            }
            let mut next = Vec::new();
            for e in evs.drain(..) {
                if let Event::Fill { size, fee, .. } = &e {
                    ledger.on_fill(*size, *fee);
                }
                self.on_event_into(e.clone(), now_ms, &mut cmds);
                next.extend_from_slice(&cmds);
                exec.push(e);
            }
            sent.extend_from_slice(&next);
            evs = venue.ingest_commands(&next, now_ms);
        }
        (sent, exec)
    }
}
