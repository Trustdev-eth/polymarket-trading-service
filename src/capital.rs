//! Per-user size caps, house-best presets, and necessary-fund sheet.
//! Fund scales with betting size: 5 shares → $200, 50 shares → $7000.

use serde::Serialize;

use crate::config::{
    HOUSE_BASE, HOUSE_NET, MIN_BASE, MONTH_BASE, MONTH_NET, TEST_BASE, TEST_NET,
};

const MIN_FUND: f64 = 200.0;
const FUND_AT_MONTH_CAP: f64 = 7000.0;

/// Linear from (MIN_BASE, $200) to (MONTH_BASE, $7000).
pub fn fund_for_base(base: f64) -> f64 {
    let b = base.max(MIN_BASE);
    let span = MONTH_BASE - MIN_BASE;
    if span <= 0.0 {
        return MIN_FUND;
    }
    let t = ((b - MIN_BASE) / span).clamp(0.0, 1.0);
    let raw = MIN_FUND + t * (FUND_AT_MONTH_CAP - MIN_FUND);
    (raw / 50.0).round() * 50.0
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct PlanCaps {
    pub plan: &'static str,
    pub base: f64,
    #[serde(rename = "maxNet")]
    pub max_net: f64,
    pub hedge: bool,
}

pub fn caps_for_plan(plan: &str) -> PlanCaps {
    match plan {
        "month" => PlanCaps {
            plan: "month",
            base: MONTH_BASE,
            max_net: MONTH_NET,
            hedge: false,
        },
        _ => PlanCaps {
            plan: "test",
            base: TEST_BASE,
            max_net: TEST_NET,
            hedge: false,
        },
    }
}

pub fn house_best(plan: &str) -> PlanCaps {
    PlanCaps {
        plan: if plan == "month" { "month" } else { "test" },
        base: HOUSE_BASE,
        max_net: HOUSE_NET,
        hedge: false,
    }
}

fn round1(n: f64) -> f64 {
    (n * 10.0).round() / 10.0
}

fn round2(n: f64) -> f64 {
    (n * 100.0).round() / 100.0
}

/// BASE 1‥cap, MAX_NET BASE‥cap, hedge free.
pub fn clamp_settings(
    plan: &str,
    base: f64,
    max_net: f64,
    hedge: bool,
) -> Result<(f64, f64, bool), String> {
    if !base.is_finite() || !max_net.is_finite() {
        return Err("Size must be a number.".into());
    }
    let caps = caps_for_plan(plan);
    let base = round1(base).clamp(MIN_BASE, caps.base);
    // MAX_NET=0 means uncapped (hold FAK inventory to settlement).
    let max_net = if max_net <= 0.0 {
        0.0
    } else {
        round1(max_net).clamp(base, caps.max_net)
    };
    Ok((base, max_net, hedge))
}

#[derive(Clone, Debug, Serialize)]
pub struct CapitalSheet {
    pub base: f64,
    #[serde(rename = "maxNet")]
    pub max_net: f64,
    pub hedge: bool,
    /// pUSD to hold MAX_NET shares at $1 (worst binary).
    #[serde(rename = "fundUsd")]
    pub fund_usd: f64,
    /// One FAK at the worst price ($1 / share).
    #[serde(rename = "perTakeWorstUsd")]
    pub per_take_worst_usd: f64,
    /// One FAK around a 50¢ touch.
    #[serde(rename = "perTakeTypicalUsd")]
    pub per_take_typical_usd: f64,
    #[serde(rename = "feeBufferUsd")]
    pub fee_buffer_usd: f64,
    /// What to keep in the funder: inventory cap + fee buffer.
    #[serde(rename = "totalUsd")]
    pub total_usd: f64,
    #[serde(rename = "housePnlUsd")]
    pub house_pnl_usd: f64,
    #[serde(rename = "houseBase")]
    pub house_base: f64,
    /// House paper PnL × (your BASE / house BASE).
    #[serde(rename = "expectedPnlUsd")]
    pub expected_pnl_usd: f64,
    /// expected / total × 100.
    #[serde(rename = "roiPct")]
    pub roi_pct: f64,
}

pub fn sheet(
    base: f64,
    max_net: f64,
    hedge: bool,
    house_pnl_usd: f64,
    house_base: f64,
) -> CapitalSheet {
    let base = base.max(MIN_BASE);
    let max_net = max_net.max(base);
    let total = fund_for_base(base);
    let fund = total;
    let per_worst = round2(base);
    let per_typ = round2(base * 0.5);
    let fee = 0.0;
    let hb = house_base.max(MIN_BASE);
    let expected = round2(house_pnl_usd * (base / hb));
    let roi = if total > 0.0 {
        round2(expected / total * 100.0)
    } else {
        0.0
    };
    CapitalSheet {
        base: round1(base),
        max_net: round1(max_net),
        hedge,
        fund_usd: fund,
        per_take_worst_usd: per_worst,
        per_take_typical_usd: per_typ,
        fee_buffer_usd: fee,
        total_usd: total,
        house_pnl_usd: round2(house_pnl_usd),
        house_base: round1(hb),
        expected_pnl_usd: expected,
        roi_pct: roi,
    }
}

/// Suggested MAX_NET for a chosen BASE, same ratio as house best, inside the cap.
pub fn net_for_base(plan: &str, base: f64) -> f64 {
    if HOUSE_NET <= 0.0 {
        return 0.0;
    }
    let caps = caps_for_plan(plan);
    let base = round1(base).clamp(MIN_BASE, caps.base);
    let ratio = caps.max_net / caps.base;
    round1((base * ratio).clamp(base, caps.max_net))
}

pub fn ladder(
    plan: &str,
    user_base: f64,
    user_net: f64,
    user_hedge: bool,
    house_pnl_usd: f64,
) -> Vec<CapitalSheet> {
    let caps = caps_for_plan(plan);
    let step = if caps.base <= 2.5 { 0.5 } else { 1.0 };
    let mut sizes = Vec::new();
    let mut b = MIN_BASE;
    while b <= caps.base + 1e-9 {
        sizes.push(round1(b));
        b += step;
    }
    let ub = round1(user_base);
    if ub >= MIN_BASE && sizes.iter().all(|x| (*x - ub).abs() >= 0.05) {
        sizes.push(ub);
        sizes.sort_by(|a, b| a.partial_cmp(b).unwrap());
    }
    sizes
        .into_iter()
        .map(|b| {
            let mine = (b - ub).abs() < 0.05;
            let net = if mine {
                user_net.max(b)
            } else {
                net_for_base(plan, b)
            };
            sheet(
                b,
                net,
                if mine { user_hedge } else { true },
                house_pnl_usd,
                caps.base,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plan_caps() {
        let c = caps_for_plan("test");
        assert_eq!(c.base, 5.0);
        assert_eq!(c.max_net, 15.0);
        let m = caps_for_plan("month");
        assert_eq!(m.base, 50.0);
        assert_eq!(m.max_net, 150.0);
        let best = house_best("month");
        assert_eq!(best.base, 5.0);
        assert_eq!(best.max_net, 0.0);
        assert!(!best.hedge);
    }

    #[test]
    fn clamp_respects_plan_ceiling() {
        let (b, n, h) = clamp_settings("test", 9.0, 40.0, false).unwrap();
        assert_eq!(b, 5.0);
        assert_eq!(n, 15.0);
        assert!(!h);
    }

    #[test]
    fn clamp_lifts_below_clob_min() {
        let (b, n, _) = clamp_settings("month", 0.2, 1.0, true).unwrap();
        assert_eq!(b, 5.0);
        assert!(n >= 5.0);
    }

    #[test]
    fn fund_follows_betting_size() {
        let s = sheet(5.0, 15.0, true, 12.0, 5.0);
        assert_eq!(s.total_usd, 200.0);
        assert_eq!(s.fund_usd, 200.0);
        let maxed = sheet(50.0, 150.0, true, 0.0, 5.0);
        assert_eq!(maxed.total_usd, 7000.0);
        let lifted = sheet(2.0, 6.0, true, 0.0, 2.0);
        assert_eq!(lifted.total_usd, 200.0);
        assert_eq!(fund_for_base(6.0), 350.0);
    }

    #[test]
    fn scales_house_pnl_with_base() {
        let s = sheet(10.0, 30.0, true, 10.0, 5.0);
        assert_eq!(s.expected_pnl_usd, 20.0);
        assert_eq!(s.total_usd, fund_for_base(10.0));
    }

    #[test]
    fn net_follows_house_ratio() {
        assert_eq!(net_for_base("month", 5.0), 0.0);
        assert_eq!(net_for_base("test", 5.0), 0.0);
    }

    #[test]
    fn ladder_starts_at_clob_min() {
        let rows = ladder("month", 5.0, 15.0, true, 10.0);
        assert_eq!(rows.first().unwrap().base, 5.0);
        assert_eq!(rows.first().unwrap().total_usd, 200.0);
        assert_eq!(rows.last().unwrap().base, 50.0);
        assert_eq!(rows.last().unwrap().total_usd, 7000.0);
    }
}
