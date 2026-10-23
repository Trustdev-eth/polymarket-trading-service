//! CLOB V2 client: cached TYPE_HASH/domain, secp256k1 ECDSA, L1/L2 auth.
//! Tick / fee / neg-risk are pre-resolved; never fetched on the order path.

pub mod amounts;
pub mod auth;
pub mod eip712;
pub mod http;
pub mod limiter;
pub mod presign;
pub mod sign;

pub use amounts::{order_amounts, RoundConfig};
pub use auth::{build_hmac, l1_headers, l2_headers, ApiCreds};
pub use eip712::{
    order_v1_type_hash, order_v2_type_hash, parse_address, u256_from_dec, u256_to_dec, CachedDomain,
    EXCHANGE_V2, NEG_RISK_EXCHANGE_V2, SIG_TYPE_POLY_1271,
};
pub use http::{fetch_match_delay, fetch_tick_fee_negrisk, ClobClient, FastCancelCache, MarketResolved, CLOB_HOST};
pub use limiter::DualLimiter;
pub use presign::{sign_grid, PresignEntry, PresignMarket, PresignPool, PRESIGN_SHAPES};
pub use sign::{OrderV2, SignError, V2Signer};
