//! Tick-size rounding matching py-clob-client-v2 `ROUNDING_CONFIG`.
//! Never call CLOB GET helpers here — tick is pre-resolved.

#[derive(Clone, Copy, Debug)]
pub struct RoundConfig {
    pub price: i32,
    pub size: i32,
    pub amount: i32,
}

pub fn round_config(tick: f64) -> RoundConfig {
    if tick >= 0.1 - 1e-12 {
        RoundConfig {
            price: 1,
            size: 2,
            amount: 3,
        }
    } else if tick >= 0.01 - 1e-12 {
        RoundConfig {
            price: 2,
            size: 2,
            amount: 4,
        }
    } else if tick >= 0.001 - 1e-12 {
        RoundConfig {
            price: 3,
            size: 2,
            amount: 5,
        }
    } else {
        RoundConfig {
            price: 4,
            size: 2,
            amount: 6,
        }
    }
}

pub fn round_normal(x: f64, sig: i32) -> f64 {
    let p = 10f64.powi(sig);
    (x * p).round() / p
}

pub fn round_down(x: f64, sig: i32) -> f64 {
    let p = 10f64.powi(sig);
    (x * p).floor() / p
}

pub fn to_token_decimals(x: f64) -> u128 {
    (x * 1_000_000.0).round() as u128
}

/// BUY: makerAmount = USDC (size*price), takerAmount = shares.
/// SELL: makerAmount = shares, takerAmount = USDC.
#[derive(Clone, Copy, Debug)]
pub struct Amounts {
    pub side: u8, // 0 buy 1 sell
    pub maker: u128,
    pub taker: u128,
}

pub fn order_amounts(is_buy: bool, size: f64, price: f64, tick: f64) -> Amounts {
    let rc = round_config(tick);
    let raw_price = round_normal(price, rc.price);
    if is_buy {
        let raw_taker = round_down(size, rc.size);
        let raw_maker = raw_taker * raw_price;
        Amounts {
            side: 0,
            maker: to_token_decimals(raw_maker),
            taker: to_token_decimals(raw_taker),
        }
    } else {
        let raw_maker = round_down(size, rc.size);
        let raw_taker = raw_maker * raw_price;
        Amounts {
            side: 1,
            maker: to_token_decimals(raw_maker),
            taker: to_token_decimals(raw_taker),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buy_five_at_fifty_one_cents() {
        let a = order_amounts(true, 5.0, 0.51, 0.01);
        assert_eq!(a.side, 0);
        assert_eq!(a.taker, 5_000_000);
        assert_eq!(a.maker, 2_550_000);
    }
}
