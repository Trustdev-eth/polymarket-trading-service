//! Course math (polycommons / polytake) plus Polymarket fee and TWAP lock-in fair.

pub const LEADER_CORR: f64 = 0.7;
pub const SHIFT_ALPHA: f64 = 0.04;
pub const DELTA_EWMA_ALPHA: f64 = 0.3;
pub const FAST_VAR_ALPHA: f64 = 0.06452;
pub const SLOW_VAR_ALPHA: f64 = 0.02198;
/// 0.3 vol annualized, converted to 10s samples.
pub const INIT_VAR: f64 = (0.3 * 0.3) / (365.0 * 24.0 * 60.0 * 6.0);
pub const MIN_SIGEST: f64 = 0.00003;
pub const FEE_CONSTANT: f64 = 0.25;
pub const FEE_EXP: f64 = 2.0;
const INV_SQRT_2PI: f64 = 0.398_942_280_401_432_7;

/// Ŝ = m − μ − ρδ.
#[inline]
pub fn shift_oracle_price(mid: f64, shift_ewma: f64, delta_ewma: f64) -> f64 {
    mid - shift_ewma - delta_ewma * LEADER_CORR
}

#[inline]
pub fn ewma(alpha: f64, x: f64, prev: f64) -> f64 {
    alpha * x + (1.0 - alpha) * prev
}

/// τ-mixture of the fast and slow vols.
pub fn vol_mixture(vol_fast: f64, vol_slow: f64, tau: f64, tau_min: f64, tau_max: f64) -> f64 {
    let w = ((tau_max - tau) / (tau_max - tau_min)).clamp(0.0, 1.0);
    w * vol_fast + (1.0 - w) * vol_slow
}

/// σ̂ per √second from EWMA variances of 10s log returns.
pub fn sigest_from_ewma(var_fast: f64, var_slow: f64, tau: f64, tau_min: f64, tau_max: f64) -> f64 {
    let vol_fast = (var_fast / 10.0).sqrt();
    let vol_slow = (var_slow / 10.0).sqrt();
    vol_mixture(vol_fast, vol_slow, tau, tau_min, tau_max).max(MIN_SIGEST)
}

fn valid(s: f64, k: f64, tau: f64, sigma: f64) -> bool {
    tau > 0.0 && sigma > 0.0 && s > 0.0 && k > 0.0
}

pub fn d2(s: f64, k: f64, tau: f64, sigma: f64) -> f64 {
    if !valid(s, k, tau, sigma) {
        return 0.0;
    }
    ((s / k).ln() - 0.5 * sigma * sigma * tau) / (sigma * tau.sqrt())
}

/// φ(d2).
pub fn nd2(s: f64, k: f64, tau: f64, sigma: f64) -> f64 {
    if !valid(s, k, tau, sigma) {
        return 0.0;
    }
    let x = d2(s, k, tau, sigma);
    INV_SQRT_2PI * (-0.5 * x * x).exp()
}

/// Digital delta scale φ(d2) / (σ√τ): price change per unit dS/S.
pub fn nd2_scaled(s: f64, k: f64, tau: f64, sigma: f64) -> f64 {
    let n = nd2(s, k, tau, sigma);
    if n == 0.0 {
        0.0
    } else {
        n / (sigma * tau.sqrt())
    }
}

/// Cash-or-nothing fair Φ(d2).
pub fn digital_phi(s: f64, k: f64, tau: f64, sigma: f64) -> f64 {
    if tau <= 0.0 {
        return if s >= k { 1.0 } else { 0.0 };
    }
    if !valid(s, k, tau, sigma) {
        return 0.5;
    }
    norm_cdf(d2(s, k, tau, sigma))
}

/// P(w·S_locked + (1−w)·S_future ≥ K) during the 60s settlement TWAP.
pub fn twap_fair(locked_s: f64, locked_w: f64, spot: f64, k: f64, tau_rem: f64, sigma: f64) -> f64 {
    let w = locked_w.clamp(0.0, 1.0);
    if w >= 1.0 || tau_rem <= 0.0 {
        return if locked_s >= k { 1.0 } else { 0.0 };
    }
    if w <= 0.0 {
        return digital_phi(spot, k, tau_rem, sigma);
    }
    let k_eff = (k - w * locked_s) / (1.0 - w);
    if k_eff <= 0.0 {
        return 1.0;
    }
    digital_phi(spot, k_eff, tau_rem, sigma)
}

pub fn norm_cdf(x: f64) -> f64 {
    0.5 * (1.0 + erf(x / std::f64::consts::SQRT_2))
}

/// Abramowitz–Stegun 7.1.26.
fn erf(x: f64) -> f64 {
    let sign = x.signum();
    let x = x.abs();
    let t = 1.0 / (1.0 + 0.3275911 * x);
    let poly = ((((1.061405429 * t - 1.453152027) * t + 1.421413741) * t - 0.284496736) * t
        + 0.254829592)
        * t;
    sign * (1.0 - poly * (-x * x).exp())
}

#[inline]
pub fn ceil_cent(p: f64) -> i32 {
    (p * 100.0 - 1e-9).ceil() as i32
}

#[inline]
pub fn floor_cent(p: f64) -> i32 {
    (p * 100.0 + 1e-9).floor() as i32
}

/// Round p toward 0.5 for a conservative taker fee estimate.
pub fn bias_towards_half(p: f64) -> f64 {
    if p < 0.5 {
        (p * 100.0).ceil() / 100.0
    } else {
        (p * 100.0).floor() / 100.0
    }
}

/// Round p away from 0.5 for a conservative maker rebate estimate.
pub fn bias_away_from_half(p: f64) -> f64 {
    if p < 0.5 {
        (p * 100.0).floor() / 100.0
    } else {
        (p * 100.0).ceil() / 100.0
    }
}

/// Lecture crypto fee 0.25 [p(1−p)]². Always in the take/hedge gate (Kalshi always
/// charges `taker_fee`). CLOB V2 often reports 0 bps while this curve still holds.
pub fn compute_eff_fee(p_biased: f64, _fee_bps: u32) -> f64 {
    let p = p_biased.clamp(0.01, 0.99);
    FEE_CONSTANT * (p * (1.0 - p)).powf(FEE_EXP)
}

/// CLOB tick in cents. Sub-cent ticks still snap to 1¢ because intents are cent-priced.
#[inline]
pub fn tick_cents(tick_size: f64) -> i32 {
    if tick_size <= 0.0 {
        1
    } else {
        ((tick_size * 100.0).round() as i32).max(1)
    }
}

#[inline]
pub fn ceil_tick(p: f64, tick_cents: i32) -> i32 {
    let t = tick_cents.max(1);
    let c = ceil_cent(p);
    let rem = c.rem_euclid(t);
    if rem == 0 {
        c
    } else {
        c + t - rem
    }
}

#[inline]
pub fn floor_tick(p: f64, tick_cents: i32) -> i32 {
    let t = tick_cents.max(1);
    let c = floor_cent(p);
    c - c.rem_euclid(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shift_oracle_matches_course() {
        let s_hat = shift_oracle_price(100_000.0, 12.0, 5.0);
        assert!((s_hat - (100_000.0 - 12.0 - 5.0 * 0.7)).abs() < 1e-12);
    }

    #[test]
    fn vol_mixture_weights() {
        assert!((vol_mixture(0.2, 0.1, 100.0, 100.0, 300.0) - 0.2).abs() < 1e-12);
        assert!((vol_mixture(0.2, 0.1, 300.0, 100.0, 300.0) - 0.1).abs() < 1e-12);
        assert!((vol_mixture(0.2, 0.1, 200.0, 100.0, 300.0) - 0.15).abs() < 1e-12);
        assert!((vol_mixture(0.2, 0.1, 50.0, 100.0, 300.0) - 0.2).abs() < 1e-12);
        assert!((vol_mixture(0.2, 0.1, 400.0, 100.0, 300.0) - 0.1).abs() < 1e-12);
    }

    #[test]
    fn init_var_and_sigest_floor() {
        assert!((INIT_VAR - 2.8538812785388127e-8).abs() < 1e-20);
        let s = sigest_from_ewma(INIT_VAR, INIT_VAR, 300.0, 100.0, 300.0);
        assert!((s - (INIT_VAR / 10.0).sqrt()).abs() < 1e-12);
    }

    #[test]
    fn nd2_atm() {
        let n = nd2(68_825.9, 68_825.9, 300.0, 0.00003);
        assert!((n - INV_SQRT_2PI).abs() < 1e-6);
        let scaled = nd2_scaled(68_825.9, 68_825.9, 300.0, 0.00003);
        assert!((scaled - 767.38).abs() < 0.5);
        assert_eq!(nd2(0.0, 1.0, 1.0, 1.0), 0.0);
        assert_eq!(nd2(1.0, 1.0, 0.0, 1.0), 0.0);
    }

    #[test]
    fn digital_and_twap_fair() {
        assert!((digital_phi(100.0, 100.0, 300.0, 0.00003) - 0.5).abs() < 5e-4);
        assert_eq!(twap_fair(101.0, 1.0, 100.0, 100.0, 0.0, 0.01), 1.0);
        assert_eq!(twap_fair(99.0, 1.0, 100.0, 100.0, 0.0, 0.01), 0.0);
        let a = twap_fair(0.0, 0.0, 100.0, 100.0, 300.0, 0.00003);
        assert!((a - digital_phi(100.0, 100.0, 300.0, 0.00003)).abs() < 1e-12);
        assert!(twap_fair(110.0, 0.5, 100.0, 100.0, 30.0, 0.0001) > 0.9);
        assert!(twap_fair(90.0, 0.5, 100.0, 100.0, 30.0, 0.0001) < 0.1);
    }

    #[test]
    fn cents_and_fee() {
        assert_eq!(floor_cent(0.514), 51);
        assert_eq!(ceil_cent(0.514), 52);
        assert_eq!(ceil_cent(0.51), 51);
        assert_eq!(floor_cent(0.57), 57);
        assert!((compute_eff_fee(0.5, 0) - 0.015625).abs() < 1e-12);
        assert!((compute_eff_fee(0.5, 1) - 0.015625).abs() < 1e-12);
        assert_eq!(tick_cents(0.01), 1);
        assert_eq!(ceil_tick(0.514, 1), 52);
        assert_eq!(floor_tick(0.514, 1), 51);
        assert!((bias_towards_half(0.514) - 0.51).abs() < 1e-12);
        assert!((bias_towards_half(0.486) - 0.49).abs() < 1e-12);
    }
}
