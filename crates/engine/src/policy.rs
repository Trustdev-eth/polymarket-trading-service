//! GTR quotes, FAK takes at the touch, and the 0.5s post-only inventory hedge.

use crate::bands::{Band, Bands};
use crate::engine::Engine;
use crate::math::{bias_towards_half, ceil_tick, compute_eff_fee, floor_tick};
use crate::types::{CancelReason, Command, IntentKind, OrderIntent, Side, TakeSkip, Tif, TokenRef};

const MIN_HEDGE_QTY: f64 = 1.0;

struct Quotes {
    bid: f64,
    ask: f64,
    fak_long: f64,
    fak_short: f64,
    ds_s_long: f64,
    ds_s_short: f64,
}

impl Engine {
    pub(crate) fn policy(&mut self, now_ms: u64, out: &mut Vec<Command>) {
        if now_ms < self.state.halt_until_ms {
            self.oms.cancel_all(CancelReason::Halt, now_ms, out);
            return;
        }
        let tau = self.state.tau_s(now_ms);
        if tau <= self.cfg.no_hedge_s {
            self.oms.cancel_all(CancelReason::NoQuote, now_ms, out);
            return;
        }
        let mut buf = [Band::default(); Bands::CAP];
        let n = self.bands.valid(now_ms, &mut buf);
        if n == 0 {
            if self.state.book_mid > 0.0 {
                self.state.allow = crate::types::AllowTrade::WaitingBook;
                self.oms.cancel_all(CancelReason::StaleBand, now_ms, out);
            }
            return;
        }
        if self.state.s_hat <= 0.0 {
            return;
        }

        let q = self.quotes(self.state.s_hat, &buf[..n]);
        let oracle_ok = self.oracle_fresh_for_policy(now_ms);
        let s = &mut self.state;
        s.quote_ts_ms = now_ms;
        s.quote_bid = q.bid;
        s.quote_ask = q.ask;
        s.fak_long = q.fak_long;
        s.fak_short = q.fak_short;
        s.ds_s_long = q.ds_s_long;
        s.ds_s_short = q.ds_s_short;

        if !s.allow.is_ready(now_ms) || !oracle_ok {
            return;
        }
        s.allow = crate::types::AllowTrade::Ready;
        if tau > self.cfg.no_quote_s {
            self.maybe_take(now_ms, out);
        }
        self.maybe_hedge(now_ms, out);
    }

    /// Course GTR quotes over every live band; FAK prices use the clipped dS.
    fn quotes(&self, s_hat: f64, bands: &[Band]) -> Quotes {
        let cfg = &self.cfg;
        let clip = s_hat * cfg.clip_frac;
        let mut q = Quotes {
            bid: f64::MAX,
            ask: f64::MIN,
            fak_long: f64::MAX,
            fak_short: f64::MIN,
            ds_s_long: 0.0,
            ds_s_short: 0.0,
        };
        for b in bands {
            let d_ask = s_hat - b.ref_min;
            let d_bid = s_hat - b.ref_max;
            let (nd2_ask, beta_ask) = if d_ask > 0.0 {
                (b.nd2_scaled_max, cfg.beta_high)
            } else {
                (b.nd2_scaled_min, cfg.beta_low)
            };
            let (nd2_bid, beta_bid) = if d_bid < 0.0 {
                (b.nd2_scaled_max, cfg.beta_high)
            } else {
                (b.nd2_scaled_min, cfg.beta_low)
            };
            q.ask = q.ask.max(b.mid + beta_ask * nd2_ask * d_ask / b.likely_ref);
            q.bid = q.bid.min(b.mid + beta_bid * nd2_bid * d_bid / b.likely_ref);

            let scale = cfg.beta_low * b.nd2_scaled_min / b.likely_ref;
            q.fak_long = q.fak_long.min(b.mid + scale * d_bid.min(clip));
            q.fak_short = q.fak_short.max(b.mid + scale * d_ask.max(-clip));
        }
        let cur = bands[0];
        q.ds_s_long = (s_hat - cur.ref_max).min(clip) / cur.likely_ref;
        q.ds_s_short = (s_hat - cur.ref_min).max(-clip) / cur.likely_ref;
        q
    }

    /// Lift the stale side only when the touch is at or through model price − fee.
    fn maybe_take(&mut self, now_ms: u64, out: &mut Vec<Command>) {
        let s = &self.state;
        let cfg = &self.cfg;
        if now_ms.saturating_sub(s.last_take_ms) < cfg.taker_retry_ms || self.oms.has_kind(IntentKind::Taker) {
            return;
        }
        let net = s.position.net();
        let cap_n = cfg.net_cap();
        let want_long = s.ds_s_long > cfg.taker_ds_s;
        let want_short = s.ds_s_short < -cfg.taker_ds_s;
        let tick = self.tick_cents();

        let (take, skip) = if want_long && net < cap_n && s.yes_ask > 0.0 {
            let touch = ceil_tick(s.yes_ask, tick);
            let cap = floor_tick(s.fak_long - self.fee(s.yes_ask), tick);
            let px = (touch + cfg.taker_slack_cents).min(cap);
            let skip = TakeSkip {
                token: TokenRef::Yes,
                touch,
                cap,
                ds_s: s.ds_s_long,
            };
            (
                (px >= touch && self.px_in_range(px)).then(|| (TokenRef::Yes, px, px, (cap_n - net).min(cfg.base_amount))),
                Some(skip),
            )
        } else if want_short && net > -cap_n && s.yes_bid > 0.0 {
            let touch = floor_tick(s.yes_bid, tick);
            let floor = ceil_tick(s.fak_short + self.fee(s.yes_bid), tick);
            let yes_px = (touch - cfg.taker_slack_cents).max(floor);
            let no_px = 100 - yes_px;
            let skip = TakeSkip {
                token: TokenRef::No,
                touch: 100 - touch,
                cap: 100 - floor,
                ds_s: s.ds_s_short,
            };
            (
                (yes_px <= touch && self.px_in_range(no_px)).then(|| (TokenRef::No, no_px, -yes_px, (cap_n + net).min(cfg.base_amount))),
                Some(skip),
            )
        } else {
            (None, None)
        };
        if take.is_none() && skip.is_some_and(|k| self.px_in_range(k.touch)) {
            self.state.skip = skip;
        }

        if !want_long && !want_short {
            self.state.last_take_p = 0;
        }
        let Some((token, px, tag, size)) = take else {
            return;
        };
        if tag == self.state.last_take_p {
            return;
        }
        self.state.last_take_p = tag;
        self.state.last_take_ms = now_ms;
        if let Some(h) = self.oms.live(IntentKind::Hedge).filter(|h| h.token == token) {
            self.oms.request_cancel(h.client_seq, CancelReason::Replace, now_ms, out);
        }
        self.emit(token, px, size, Tif::Fak, IntentKind::Taker, now_ms, out);
    }

    /// One resting post-only hedge at a time, re-priced every `hedge_tick_ms`, never larger than |net|.
    fn maybe_hedge(&mut self, now_ms: u64, out: &mut Vec<Command>) {
        if !self.cfg.hedge {
            self.oms.cancel_kind(IntentKind::Hedge, CancelReason::Halt, now_ms, out);
            return;
        }
        let cfg = &self.cfg;
        let s = &self.state;
        if now_ms.saturating_sub(s.last_hedge_ms) < cfg.hedge_tick_ms
            || now_ms.saturating_sub(s.quote_ts_ms) > cfg.quote_fresh_ms
            || self.oms.in_flight(IntentKind::Hedge)
        {
            return;
        }
        let net = s.position.net();
        let need = (net.abs() * 100.0).floor() / 100.0;
        let size = need.min(cfg.hedge_amount);
        let clip = cfg.hedge_amount.max(1e-9);
        let eff = (cfg.hedge_dist - need / clip * cfg.inventory_k).max(-cfg.max_nehd) / 100.0;
        let tick = self.tick_cents();
        let (token, px) = if net < 0.0 {
            (
                TokenRef::Yes,
                floor_tick(s.quote_bid - self.fee(s.quote_bid) - eff, tick).min(ceil_tick(s.yes_ask, tick) - tick),
            )
        } else {
            (
                TokenRef::No,
                100 - ceil_tick(s.quote_ask + self.fee(s.quote_ask) + eff, tick).max(floor_tick(s.yes_bid, tick) + tick),
            )
        };
        self.state.last_hedge_ms = now_ms;

        let live = self.oms.live(IntentKind::Hedge);
        let wanted = need >= MIN_HEDGE_QTY && self.px_in_range(px);
        if let Some(h) = live {
            if wanted && h.token == token && h.px_ticks == px && h.size <= size + 1e-9 {
                return;
            }
            self.oms.request_cancel(h.client_seq, CancelReason::Replace, now_ms, out);
            return;
        }
        if wanted {
            self.emit(token, px, size, Tif::Gtc, IntentKind::Hedge, now_ms, out);
        }
    }

    fn fee(&self, p: f64) -> f64 {
        let bps = self.state.info.as_ref().map(|c| c.fee_bps).unwrap_or(0);
        compute_eff_fee(bias_towards_half(p), bps)
    }

    pub(crate) fn tick_cents(&self) -> i32 {
        crate::math::tick_cents(self.state.info.as_ref().map(|c| c.tick_size).unwrap_or(0.01))
    }

    /// Inclusive tick-aware rails: default 1¢ tick → 1..=99.
    pub(crate) fn px_in_range(&self, px: i32) -> bool {
        let tick = self.tick_cents();
        let lo = self.cfg.px_lo.max(tick);
        let hi = self.cfg.px_hi.min(100 - tick);
        px >= lo && px <= hi
    }

    fn emit(&mut self, token: TokenRef, px: i32, size: f64, tif: Tif, kind: IntentKind, now_ms: u64, out: &mut Vec<Command>) {
        let intent = OrderIntent {
            token,
            side: Side::Buy,
            px_ticks: px,
            size,
            tif,
            post_only: kind == IntentKind::Hedge,
            created_ms: now_ms,
            client_seq: self.oms.alloc_seq(),
            kind,
        };
        if self.oms.submit(intent, now_ms) {
            out.push(Command::Submit(intent));
        }
    }
}
