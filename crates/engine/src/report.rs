//! Read-only views for logs and the dashboard.

use serde::Serialize;

use crate::engine::Engine;
use crate::math::{digital_phi, twap_fair};
use crate::types::AllowTrade;

/// Market + inventory snapshot written with every order / fill and sampled for the dashboard.
#[derive(Clone, Debug, Serialize)]
pub struct MarketSnap {
    pub slug: String,
    pub utc0_ms: u64,
    pub utc1_ms: u64,
    pub k_t: f64,
    pub tau_s: f64,
    pub regime: &'static str,
    pub quote: &'static str,
    pub s_hat: f64,
    pub binance_mid: f64,
    pub chainlink: f64,
    pub twap: f64,
    pub fair: Option<f64>,
    pub yes_bid: f64,
    pub yes_ask: f64,
    pub book_mid: f64,
    pub book_age_ms: u64,
    pub gtr_bid: f64,
    pub gtr_ask: f64,
    pub nd2_scaled_max: f64,
    pub band_ref_min: f64,
    pub band_ref_max: f64,
    pub ds_s_long: f64,
    pub ds_s_short: f64,
    pub taker_ds_s: f64,
    pub fak_long: f64,
    pub fak_short: f64,
    pub up_qty: f64,
    pub down_qty: f64,
    pub up_cost: f64,
    pub down_cost: f64,
    pub fees: f64,
    pub net: f64,
    pub mark_pnl: f64,
}

impl MarketSnap {
    pub fn signal_line(&self) -> String {
        let fair = self.fair.map(|p| format!("{p:.4}")).unwrap_or_else(|| "n/a".into());
        format!(
            "SIGNAL tau={:.0}s regime={} cl={:.2} twap={:.2} k={:.2} s_hat={:.2} fair={fair} book={:.3}/{:.3} age={}ms nd2={:.0} net={:.2} fees={:.3} mark={:+.4} quote={}",
            self.tau_s, self.regime, self.chainlink, self.twap, self.k_t, self.s_hat, self.yes_bid, self.yes_ask,
            self.book_age_ms, self.nd2_scaled_max, self.net, self.fees, self.mark_pnl, self.quote
        )
    }
}

/// Compact snapshot for the gateway SIGNAL wire frame.
#[derive(Clone, Debug)]
pub struct Signal {
    pub s_hat: f64,
    pub chainlink: f64,
    pub twap: f64,
    pub k_t: f64,
    pub tau_s: f64,
    pub fair: Option<f64>,
    pub book_mid: f64,
    pub nd2_scaled_max: f64,
    pub net: f64,
    pub mark_pnl: f64,
    pub quote: &'static str,
    pub regime: &'static str,
}

impl Signal {
    pub fn from_snap(s: &MarketSnap) -> Self {
        Self {
            s_hat: s.s_hat,
            chainlink: s.chainlink,
            twap: s.twap,
            k_t: s.k_t,
            tau_s: s.tau_s,
            fair: s.fair,
            book_mid: s.book_mid,
            nd2_scaled_max: s.nd2_scaled_max,
            net: s.net,
            mark_pnl: s.mark_pnl,
            quote: s.quote,
            regime: s.regime,
        }
    }

    pub fn line(&self) -> String {
        let fair = match self.fair {
            Some(p) if p.is_finite() => format!("{p:.4}"),
            _ => "n/a".into(),
        };
        format!(
            "SIGNAL tau={:.0}s regime={} cl={:.2} twap={:.2} k={:.2} s_hat={:.2} fair={fair} book={:.3} nd2={:.0} net={:.1} mark={:+.4} quote={}",
            self.tau_s, self.regime, self.chainlink, self.twap, self.k_t, self.s_hat, self.book_mid,
            self.nd2_scaled_max, self.net, self.mark_pnl, self.quote
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MarketResult {
    pub slug: String,
    pub k_t: f64,
    pub close: f64,
    pub winner_up: Option<bool>,
    pub yes_qty: f64,
    pub yes_cost: f64,
    pub no_qty: f64,
    pub no_cost: f64,
    pub fees: f64,
    pub pnl: Option<f64>,
}

impl MarketResult {
    pub fn line(&self) -> String {
        let winner = match self.winner_up {
            Some(true) => "Up",
            Some(false) => "Down",
            None => "unknown",
        };
        let pnl = self.pnl.map(|p| format!("{p:.4}")).unwrap_or_else(|| "n/a".into());
        let yes_avg = if self.yes_qty > 0.0 { self.yes_cost / self.yes_qty } else { 0.0 };
        let no_avg = if self.no_qty > 0.0 { self.no_cost / self.no_qty } else { 0.0 };
        format!(
            "RESULT slug={} winner={winner} k={:.2} close={:.2} up={:.2}@{:.3} down={:.2}@{:.3} pnl={pnl}",
            self.slug, self.k_t, self.close, self.yes_qty, yes_avg, self.no_qty, no_avg
        )
    }
}

impl Engine {
    /// τ > 60s: Φ(d2) on Chainlink vs K. Inside the settlement TWAP: lock-in fair.
    pub fn fair(&self, now_ms: u64) -> Option<f64> {
        let k = self.state.k_t();
        let s = self.model_s(now_ms);
        if s <= 0.0 || k <= 0.0 {
            return None;
        }
        let tau = self.state.tau_s(now_ms);
        let sigma = self.sigma(tau);
        Some(if tau > self.cfg.twap_window_s {
            digital_phi(s, k, tau, sigma)
        } else {
            let w = self.state.locked_weight(now_ms, self.cfg.twap_window_s);
            twap_fair(self.state.locked_s(), w, self.state.twap.max(s), k, tau, sigma)
        })
    }

    pub fn quote_block(&self, now_ms: u64) -> &'static str {
        let s = &self.state;
        let tau = s.tau_s(now_ms);
        if now_ms < s.halt_until_ms {
            "halt"
        } else if s.k_t() <= 0.0 {
            "no_strike"
        } else if tau <= 0.0 {
            "expired"
        } else if tau <= self.cfg.no_quote_s {
            "lock_in"
        } else if s.allow == AllowTrade::WaitingBook || (s.book_ts_ms > 0 && !self.book_fresh(now_ms)) {
            "waiting_book"
        } else if !s.allow.is_ready(now_ms) {
            "cooldown"
        } else if !self.oracle_fresh_for_policy(now_ms) {
            "bba_stale"
        } else if self.bands.newest().is_none() {
            "no_gtr"
        } else {
            "open"
        }
    }

    pub fn market_snap(&self, now_ms: u64) -> MarketSnap {
        let s = &self.state;
        let info = s.info.as_ref();
        let p = &s.position;
        let band = self.bands.newest().unwrap_or_default();
        let tau = s.tau_s(now_ms);
        MarketSnap {
            slug: info.map(|i| i.slug.clone()).unwrap_or_default(),
            utc0_ms: info.map(|i| i.utc0_ms).unwrap_or(0),
            utc1_ms: info.map(|i| i.utc1_ms).unwrap_or(0),
            k_t: s.k_t(),
            tau_s: tau,
            regime: if tau > self.cfg.twap_window_s { "spot" } else { "twap_lock" },
            quote: self.quote_block(now_ms),
            s_hat: s.s_hat,
            binance_mid: s.binance_mid,
            chainlink: s.chainlink,
            twap: s.twap,
            fair: self.fair(now_ms),
            yes_bid: s.yes_bid,
            yes_ask: s.yes_ask,
            book_mid: s.book_mid,
            book_age_ms: if s.book_ts_ms > 0 { now_ms.saturating_sub(s.book_ts_ms) } else { 0 },
            gtr_bid: s.quote_bid,
            gtr_ask: s.quote_ask,
            nd2_scaled_max: self.bands.max_nd2(),
            band_ref_min: band.ref_min,
            band_ref_max: band.ref_max,
            ds_s_long: s.ds_s_long,
            ds_s_short: s.ds_s_short,
            taker_ds_s: self.cfg.taker_ds_s,
            fak_long: s.fak_long,
            fak_short: s.fak_short,
            up_qty: p.long_qty,
            down_qty: p.short_qty,
            up_cost: p.long_cost,
            down_cost: p.short_cost,
            fees: p.fees,
            net: p.net(),
            mark_pnl: p.mark_pnl(s.book_mid),
        }
    }

    pub fn market_result(&self) -> MarketResult {
        let s = &self.state;
        let k_t = s.k_t();
        let close = [s.settle_close, if s.settle_twap > 0.0 { s.locked_s() } else { 0.0 }, s.twap]
            .into_iter()
            .find(|v| *v > 0.0)
            .unwrap_or(0.0);
        let winner_up = (k_t > 0.0 && close > 0.0).then_some(close >= k_t);
        let p = &s.position;
        MarketResult {
            slug: s.info.as_ref().map(|i| i.slug.clone()).unwrap_or_default(),
            k_t,
            close,
            winner_up,
            yes_qty: p.long_qty,
            yes_cost: p.long_cost,
            no_qty: p.short_qty,
            no_cost: p.short_cost,
            fees: p.fees,
            pnl: winner_up.map(|up| p.settle_pnl(up)),
        }
    }
}
