//! Pre-signed CLOB bodies so the hot path is a lookup + POST, not EIP-712 + ECDSA.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::http::ClobClient;

/// Order shapes the strategy sends: FAK takers and post-only GTC hedges, both buys.
/// Anything else misses the pool and is signed on demand.
pub const PRESIGN_SHAPES: [(&str, bool); 2] = [("FAK", false), ("GTC", true)];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PresignKey {
    pub token: [u8; 32],
    pub is_buy: bool,
    pub px_ticks: i32,
    pub size_e4: u32,
    /// `orderType` in the POST body. Not part of the signed struct, but it is part of the order.
    pub order_type: &'static str,
    pub post_only: bool,
}

#[derive(Clone, Debug)]
pub struct PresignEntry {
    pub body: Vec<u8>,
    pub digest: [u8; 32],
    pub cloid: String,
    pub signed_ms: u64,
}

/// The market a grid was signed for. A grid is only installed if this still matches.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PresignMarket {
    pub yes: [u8; 32],
    pub no: [u8; 32],
    pub size: f64,
}

#[derive(Clone, Debug, Default)]
pub struct PresignPool {
    pub entries: HashMap<PresignKey, PresignEntry>,
    pub size: f64,
    pub yes: [u8; 32],
    pub no: [u8; 32],
    pub ready: bool,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

pub fn size_e4(size: f64) -> u32 {
    (size * 10_000.0).round() as u32
}

impl PresignPool {
    pub fn set_market(&mut self, yes: [u8; 32], no: [u8; 32], size: f64) {
        self.yes = yes;
        self.no = no;
        self.size = size;
        self.ready = yes != [0u8; 32];
        self.entries.clear();
    }

    pub fn market(&self) -> Option<PresignMarket> {
        (self.ready && self.size > 0.0).then_some(PresignMarket {
            yes: self.yes,
            no: self.no,
            size: self.size,
        })
    }

    /// Removes and returns a fresh entry. Each signed body is POSTed at most once.
    #[allow(clippy::too_many_arguments)]
    pub fn take(
        &mut self,
        token: [u8; 32],
        is_buy: bool,
        px_ticks: i32,
        size: f64,
        order_type: &'static str,
        post_only: bool,
        now_ms: u64,
        max_age_ms: u64,
    ) -> Option<PresignEntry> {
        let key = PresignKey {
            token,
            is_buy,
            px_ticks,
            size_e4: size_e4(size),
            order_type,
            post_only,
        };
        let e = self.entries.remove(&key)?;
        (now_ms.saturating_sub(e.signed_ms) <= max_age_ms).then_some(e)
    }

    /// Replace the grid if it was signed for the current market. Returns entries installed.
    pub fn install(&mut self, market: PresignMarket, entries: HashMap<PresignKey, PresignEntry>) -> usize {
        if self.market() != Some(market) {
            return 0;
        }
        self.entries = entries;
        self.entries.len()
    }

    /// Sign and install a fresh grid in place.
    pub fn rebuild(&mut self, clob: &ClobClient, px_lo: i32, px_hi: i32) -> usize {
        let Some(m) = self.market() else {
            return 0;
        };
        let entries = sign_grid(clob, m, px_lo, px_hi);
        self.install(m, entries)
    }
}

/// Buy-side grid over `[px_lo, px_hi]` for both tokens and every `PRESIGN_SHAPES` entry.
/// CPU-bound (one ECDSA per entry); run off the async runtime.
pub fn sign_grid(
    clob: &ClobClient,
    m: PresignMarket,
    px_lo: i32,
    px_hi: i32,
) -> HashMap<PresignKey, PresignEntry> {
    let ts = now_ms();
    let size_key = size_e4(m.size);
    let cap = ((px_hi - px_lo + 1).max(0) as usize) * 2 * PRESIGN_SHAPES.len();
    let mut out = HashMap::with_capacity(cap);
    let mut n = 0u128;
    for token in [m.yes, m.no] {
        for (order_type, post_only) in PRESIGN_SHAPES {
            for px in px_lo..=px_hi {
                n += 1;
                let salt = (ts as u128) * 1_000 + n;
                let Ok((body, digest, cloid)) = clob.sign_limit(
                    token,
                    true,
                    px as f64 / 100.0,
                    m.size,
                    order_type,
                    post_only,
                    ts,
                    salt,
                    0,
                ) else {
                    continue;
                };
                out.insert(
                    PresignKey {
                        token,
                        is_buy: true,
                        px_ticks: px,
                        size_e4: size_key,
                        order_type,
                        post_only,
                    },
                    PresignEntry {
                        body,
                        digest,
                        cloid,
                        signed_ms: ts,
                    },
                );
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::ApiCreds;
    use crate::sign::V2Signer;

    const KEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
    const YES: [u8; 32] = [1u8; 32];
    const NO: [u8; 32] = [2u8; 32];

    fn client() -> ClobClient {
        let signer = V2Signer::from_hex(KEY).unwrap();
        let addr = signer.address();
        ClobClient::new(
            "http://127.0.0.1:0".into(),
            signer,
            ApiCreds { api_key: String::new(), secret: String::new(), passphrase: String::new() },
            addr,
            0,
            true,
        )
    }

    fn ready_pool() -> PresignPool {
        let mut pool = PresignPool::default();
        pool.set_market(YES, NO, 5.0);
        pool
    }

    fn order_type_of(e: &PresignEntry) -> String {
        let v: serde_json::Value = serde_json::from_slice(&e.body).unwrap();
        v["orderType"].as_str().unwrap().to_string()
    }

    #[test]
    fn fak_and_gtc_bodies_keep_their_order_type() {
        let clob = client();
        let mut pool = ready_pool();
        assert_eq!(pool.rebuild(&clob, 40, 42), 2 * 2 * 3);
        let ts = now_ms();
        let fak = pool.take(YES, true, 41, 5.0, "FAK", false, ts, 1_500).unwrap();
        assert_eq!(order_type_of(&fak), "FAK");
        let gtc = pool.take(NO, true, 41, 5.0, "GTC", true, ts, 1_500).unwrap();
        assert_eq!(order_type_of(&gtc), "GTC");
        assert_ne!(fak.digest, gtc.digest);
        assert!(pool.take(YES, true, 41, 5.0, "GTC", false, ts, 1_500).is_none(), "shape not presigned");
        assert!(pool.take(YES, false, 41, 5.0, "FAK", false, ts, 1_500).is_none(), "sells not presigned");
    }

    #[test]
    fn take_consumes_and_respects_age() {
        let clob = client();
        let mut pool = ready_pool();
        pool.rebuild(&clob, 50, 50);
        let ts = now_ms();
        assert!(pool.take(YES, true, 50, 5.0, "FAK", false, ts, 1_500).is_some());
        assert!(pool.take(YES, true, 50, 5.0, "FAK", false, ts, 1_500).is_none(), "one POST per body");
        assert!(pool.take(NO, true, 50, 5.0, "FAK", false, ts + 10_000, 1_500).is_none(), "stale");
    }

    #[test]
    fn grid_for_rolled_market_is_dropped() {
        let clob = client();
        let mut pool = ready_pool();
        let old = pool.market().unwrap();
        let grid = sign_grid(&clob, old, 50, 50);
        pool.set_market([3u8; 32], [4u8; 32], 5.0);
        assert_eq!(pool.install(old, grid), 0);
        assert!(pool.entries.is_empty());
    }

    #[test]
    #[ignore = "timing; run with --release -- --ignored --nocapture"]
    fn rebuild_timing() {
        let clob = client();
        let mut pool = ready_pool();
        let t0 = std::time::Instant::now();
        let mut n = 0;
        for _ in 0..5 {
            n = pool.rebuild(&clob, 1, 99);
        }
        let per = t0.elapsed() / 5;
        eprintln!("rebuild: {n} entries, {per:?} per rebuild, {:?} per entry", per / n as u32);
    }
}
