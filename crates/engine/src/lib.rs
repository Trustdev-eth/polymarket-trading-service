//! Deterministic Polymarket 15m taker engine. No network, no signing, no Tokio.
//!
//! Policy is FAK at the CLOB touch (capped by model − fee) plus one post-only
//! inventory hedge. `Engine::on_event` is deterministic: same events + clock ⇒
//! same commands.

mod bands;
mod bins;
mod config;
mod engine;
mod math;
mod oms;
mod paper;
mod policy;
mod report;
#[cfg(test)]
mod tests;
mod types;

pub use bands::{Band, Bands};
pub use bins::Bins;
pub use config::EngineConfig;
pub use engine::{Engine, State};
pub use math::{
    bias_away_from_half, bias_towards_half, ceil_cent, ceil_tick, compute_eff_fee, d2, digital_phi,
    ewma, floor_cent, floor_tick, nd2, nd2_scaled, shift_oracle_price, sigest_from_ewma, tick_cents,
    twap_fair, vol_mixture,
    DELTA_EWMA_ALPHA, FAST_VAR_ALPHA, INIT_VAR, LEADER_CORR, MIN_SIGEST, SHIFT_ALPHA, SLOW_VAR_ALPHA,
};
pub use oms::{Oms, SlotState};
pub use paper::{PaperBook, PaperConfig, PaperExec, PaperLedger, PaperOutcome, PaperVenue};
pub use report::{MarketResult, MarketSnap, Signal};
pub use types::{
    AllowTrade, BookDepth, CancelReason, Command, ContractInfo, Event, HaltReason, IntentKind, Level,
    OrderIntent, Position, Side, TakeSkip, Tif, TokenRef, DEPTH_LEVELS,
};
