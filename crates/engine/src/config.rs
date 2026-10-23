/// All engine knobs. IO must not live here.
#[derive(Clone, Debug)]
pub struct EngineConfig {
    pub bucket_ms: u64,
    pub fast_bin_step_ms: u64,
    pub oracle_bin_step_ms: u64,
    pub vol_tau_min: f64,
    pub vol_tau_max: f64,
    /// Seconds left at which takes stop (`NO_QUOTE`).
    pub no_quote_s: f64,
    /// Seconds left at which hedges stop and everything is cancelled (`NO_HEDGE`).
    pub no_hedge_s: f64,
    pub twap_window_s: f64,
    /// GTR lookback geometry (`FIBER_TRIP`, `ORDER_SERVE`, `TAKER_BUMP`, +30).
    pub fiber_trip_ms: u64,
    pub order_serve_ms: u64,
    pub taker_bump_ms: u64,
    pub book_extra_ms: u64,
    pub beta_low: f64,
    pub beta_high: f64,
    /// |dS| clip as a fraction of Ŝ (3bp).
    pub clip_frac: f64,
    pub gtr_ms: u64,
    /// Take only if |dS/S| exceeds this (0.00004 = 0.4bp).
    pub taker_ds_s: f64,
    /// Cents above the touch an FAK may pay, still capped by model price − fee.
    pub taker_slack_cents: i32,
    /// Minimum gap between FAKs after a miss.
    pub taker_retry_ms: u64,
    pub base_amount: f64,
    /// Rest a post-only flatten after a take. `HEDGE=0` holds FAK inventory to settlement.
    pub hedge: bool,
    pub hedge_amount: f64,
    pub hedge_tick_ms: u64,
    pub inventory_k: f64,
    pub max_nehd: f64,
    pub hedge_dist: f64,
    /// No taker that grows |net| beyond this. `MAX_NET=0` disables the cap.
    pub max_net: f64,
    pub bba_stale_ms: u64,
    /// Drop GTR / hedges if the CLOB BBO is older than this (quiet book is fine).
    pub book_stale_ms: u64,
    pub book_cooldown_ms: u64,
    pub halt_ms: u64,
    pub bba_fresh_ms: u64,
    pub quote_fresh_ms: u64,
    pub jump_frac: f64,
    /// Orders left unacked this long are dropped from the OMS.
    pub order_timeout_ms: u64,
    pub px_lo: i32,
    pub px_hi: i32,
    /// BBA ticks between log-return variance samples. Live bookTicker ≈ 100 (~10s).
    pub var_sample_ticks: u32,
}

impl EngineConfig {
    pub fn for_mins(mins: u32) -> Self {
        let base = Self {
            bucket_ms: 300_000,
            fast_bin_step_ms: 100,
            oracle_bin_step_ms: 50,
            vol_tau_min: 100.0,
            vol_tau_max: 300.0,
            no_quote_s: 40.0,
            no_hedge_s: 10.0,
            twap_window_s: 60.0,
            // One-box UK (this host): GTR lookback for Binance lag into eu-west-2, not loopback 1ms.
            // Measured 2026-10-09: Binance bookTicker age median 108ms / p90 110ms → fiber 90 (tighter lookback).
            // ORDER_SERVE from TLS-prewarm keep-alive GET /time (paper never POSTs): median 25ms.
            fiber_trip_ms: 90,
            order_serve_ms: 25,
            taker_bump_ms: 250,
            book_extra_ms: 30,
            beta_low: 0.50,
            beta_high: 1.25,
            clip_frac: 0.0003,
            gtr_ms: 3_000,
            taker_ds_s: 0.00004,
            taker_slack_cents: 0,
            taker_retry_ms: 250,
            base_amount: 5.0,
            hedge: false,
            hedge_amount: 5.0,
            hedge_tick_ms: 500,
            inventory_k: 0.3,
            max_nehd: 3.0,
            hedge_dist: 0.0,
            max_net: 0.0,
            bba_stale_ms: 5_000,
            book_stale_ms: 3_000,
            book_cooldown_ms: 5_000,
            halt_ms: 180_000,
            bba_fresh_ms: 1_000,
            quote_fresh_ms: 1_000,
            jump_frac: 0.00015,
            order_timeout_ms: 10_000,
            px_lo: 1,
            px_hi: 99,
            var_sample_ticks: 100,
        };
        match mins {
            5 => base,
            15 => Self {
                bucket_ms: 900_000,
                vol_tau_min: 300.0,
                vol_tau_max: 900.0,
                no_quote_s: 40.0,
                no_hedge_s: 15.0,
                ..base
            },
            other => panic!("unsupported MINS={other}; use 5 or 15"),
        }
    }

    /// Course `BOOK_INGEST = FIBER_TRIP + ORDER_SERVE + 10`.
    pub fn book_ingest_ms(&self) -> u64 {
        self.fiber_trip_ms + self.order_serve_ms + 10
    }

    /// Same-way inventory cap. `<= 0` means uncapped.
    pub fn net_cap(&self) -> f64 {
        if self.max_net > 0.0 {
            self.max_net
        } else {
            f64::INFINITY
        }
    }
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self::for_mins(15)
    }
}
