use serde::{Deserialize, Serialize};

/// Up (`Yes`) or Down (`No`). Every order is a buy of one of the two.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum TokenRef {
    Yes = 0,
    No = 1,
}

impl TokenRef {
    pub fn name(self) -> &'static str {
        match self {
            TokenRef::Yes => "Up",
            TokenRef::No => "Down",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum Side {
    Buy = 0,
    Sell = 1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum Tif {
    Gtc = 0,
    Gtd = 1,
    Fak = 2,
    Fok = 3,
}

impl Tif {
    pub fn is_immediate(self) -> bool {
        matches!(self, Tif::Fak | Tif::Fok)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum IntentKind {
    Taker = 0,
    Hedge = 1,
}

impl IntentKind {
    pub fn name(self) -> &'static str {
        match self {
            IntentKind::Taker => "taker",
            IntentKind::Hedge => "hedge",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct OrderIntent {
    pub token: TokenRef,
    pub side: Side,
    /// Limit in cents of the bought token (No price for `TokenRef::No`).
    pub px_ticks: i32,
    pub size: f64,
    pub tif: Tif,
    pub post_only: bool,
    pub created_ms: u64,
    pub client_seq: u64,
    pub kind: IntentKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CancelReason {
    Replace,
    Jump,
    NoQuote,
    StaleBand,
    Halt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HaltReason {
    BbaStale,
    BookMissing,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Command {
    Submit(OrderIntent),
    Cancel { client_seq: u64, reason: CancelReason },
    CancelAll { reason: CancelReason },
    Halt { until_ms: u64, reason: HaltReason },
}

/// Levels per side carried by `Event::Depth`. Takers price at the touch, so a few suffice.
pub const DEPTH_LEVELS: usize = 5;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Level {
    pub px: f64,
    pub size: f64,
}

/// Top of the YES book, best level first; unused slots have `size == 0`.
/// Down liquidity is the mirror: a YES bid at `p` is a No ask at `1 − p`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BookDepth {
    pub ts_ms: u64,
    pub bids: [Level; DEPTH_LEVELS],
    pub asks: [Level; DEPTH_LEVELS],
}

impl BookDepth {
    pub fn bids(&self) -> impl Iterator<Item = &Level> {
        self.bids.iter().take_while(|l| l.size > 0.0)
    }

    pub fn asks(&self) -> impl Iterator<Item = &Level> {
        self.asks.iter().take_while(|l| l.size > 0.0)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    BinanceBba { ts_exch_ms: u64, bid: f64, ask: f64 },
    /// Chainlink 60s TWAP (open K latch + last-minute lock-in).
    Twap { ts_ms: u64, value: f64 },
    /// Instant Chainlink spot (fair S while τ > 60s).
    Chainlink { ts_ms: u64, value: f64 },
    /// CLOB YES best bid / ask in dollars.
    Book { ts_ms: u64, yes_bid: f64, yes_ask: f64 },
    /// CLOB YES depth. Only the paper venue reads it; boxed to keep `Event` small.
    Depth(Box<BookDepth>),
    LoadContract(ContractInfo),
    /// Venue inventory for the loaded market (restart recovery).
    SeedPosition(Position),
    Ack { client_seq: u64, cloid: [u8; 32], ok: bool },
    /// `px` is the execution price of the bought token in dollars; `fee` in dollars.
    Fill { client_seq: u64, token: TokenRef, px: f64, size: f64, fee: f64, is_taker: bool },
    Reject { client_seq: u64 },
    InvariantTick,
    /// Window close (0 = use engine lock-in TWAP).
    Settle { close: f64 },
}

#[derive(Clone, Debug, PartialEq)]
pub struct ContractInfo {
    pub utc0_ms: u64,
    pub utc1_ms: u64,
    pub k_t: f64,
    pub fee_bps: u32,
    pub slug: String,
    pub tick_size: f64,
    pub neg_risk: bool,
    /// Venue hold on marketable orders before matching (Polymarket `itode`). 0 = none.
    pub taker_delay_ms: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AllowTrade {
    #[default]
    WaitingBook,
    Cooldown(u64),
    Ready,
}

impl AllowTrade {
    pub fn is_ready(self, now_ms: u64) -> bool {
        match self {
            AllowTrade::Ready => true,
            AllowTrade::Cooldown(until) => now_ms >= until,
            AllowTrade::WaitingBook => false,
        }
    }
}

/// A take signal refused because the touch was above model price − fee (cents, in `token`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TakeSkip {
    pub token: TokenRef,
    pub touch: i32,
    pub cap: i32,
    pub ds_s: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Position {
    pub long_qty: f64,
    pub short_qty: f64,
    pub long_cost: f64,
    pub short_cost: f64,
    pub fees: f64,
}

impl Position {
    pub fn net(&self) -> f64 {
        self.long_qty - self.short_qty
    }

    pub fn add(&mut self, token: TokenRef, px: f64, size: f64, fee: f64) {
        match token {
            TokenRef::Yes => {
                self.long_qty += size;
                self.long_cost += size * px;
            }
            TokenRef::No => {
                self.short_qty += size;
                self.short_cost += size * px;
            }
        }
        self.fees += fee;
    }

    /// Mark at the YES mid, after fees.
    pub fn mark_pnl(&self, yes_mid: f64) -> f64 {
        if yes_mid <= 0.0 {
            return 0.0;
        }
        yes_mid * self.long_qty + (1.0 - yes_mid) * self.short_qty
            - self.long_cost
            - self.short_cost
            - self.fees
    }

    pub fn settle_pnl(&self, up_wins: bool) -> f64 {
        let payout = if up_wins { self.long_qty } else { self.short_qty };
        payout - self.long_cost - self.short_cost - self.fees
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mark_and_settle_include_fees() {
        let mut p = Position::default();
        p.add(TokenRef::Yes, 0.56, 5.0, 0.0);
        assert!((p.mark_pnl(0.58) - 0.10).abs() < 1e-12);
        p.add(TokenRef::No, 0.39, 5.0, 0.05);
        assert!((p.settle_pnl(true) - (5.0 - 2.80 - 1.95 - 0.05)).abs() < 1e-12);
        assert!(p.net().abs() < 1e-12);
    }
}
