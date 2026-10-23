use chrono::{DateTime, Duration, Utc};

use crate::config::{HOUSE_BASE, HOUSE_NET, MONTH_DAYS, TEST_DAYS};
use crate::models::Sku;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grant {
    pub until: DateTime<Utc>,
    pub plan: &'static str,
    pub base_amount: f64,
    pub max_net: f64,
}

/// Test is always 3 days from `now`. Month is 30 days from now when converting
/// from test (unused test hours are dropped). Month renews from current expiry
/// if that expiry is still in the future.
pub fn next_access(
    sku: Sku,
    now: DateTime<Utc>,
    current_plan: &str,
    current_until: Option<DateTime<Utc>>,
) -> Grant {
    match sku {
        Sku::Test => Grant {
            until: now + Duration::days(TEST_DAYS),
            plan: "test",
            base_amount: HOUSE_BASE,
            max_net: HOUSE_NET,
        },
        Sku::Month => {
            let start = if current_plan == "month" {
                current_until.filter(|u| *u > now).unwrap_or(now)
            } else {
                now
            };
            Grant {
                until: start + Duration::days(MONTH_DAYS),
                plan: "month",
                base_amount: HOUSE_BASE,
                max_net: HOUSE_NET,
            }
        }
    }
}

pub fn in_window(access_until_ms: i64, now_ms: i64) -> bool {
    access_until_ms > now_ms
}

/// Paid (or admin) traders may run paper/live and use dash / PnL.
pub fn vip_open(admin: bool, access_until_ms: i64, now_ms: i64, blocked: bool) -> bool {
    !blocked && (admin || in_window(access_until_ms, now_ms))
}

/// Live/paper eligibility for a coordinator wave.
pub fn wave_eligible(access_until_ms: i64, now_ms: i64, halted: bool, blocked: bool) -> bool {
    in_window(access_until_ms, now_ms) && !halted && !blocked
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunFlags {
    pub halted: bool,
    pub paper: bool,
    pub live: bool,
}

impl RunFlags {
    pub fn stopped() -> Self {
        Self {
            halted: true,
            paper: false,
            live: false,
        }
    }

    pub fn paper() -> Self {
        Self {
            halted: false,
            paper: true,
            live: false,
        }
    }

    pub fn live() -> Self {
        Self {
            halted: false,
            paper: false,
            live: true,
        }
    }

    pub fn paper_running(self) -> bool {
        !self.halted && self.paper && !self.live
    }

    pub fn live_running(self) -> bool {
        !self.halted && self.live
    }

    pub fn status(self) -> &'static str {
        if self.live_running() {
            "live"
        } else if self.paper_running() {
            "paper"
        } else {
            "stopped"
        }
    }
}

/// Start one mode (and stop the other). Stop either mode leaves both stopped.
pub fn next_run(mode: &str, running: bool) -> Result<RunFlags, &'static str> {
    match (mode, running) {
        ("paper", true) => Ok(RunFlags::paper()),
        ("live", true) => Ok(RunFlags::live()),
        ("paper", false) | ("live", false) => Ok(RunFlags::stopped()),
        _ => Err("mode must be paper or live."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 21, 12, 0, 0).unwrap()
    }

    #[test]
    fn test_sku_is_three_days() {
        let g = next_access(Sku::Test, t(), "none", None);
        assert_eq!(g.plan, "test");
        assert_eq!((g.until - t()).num_days(), 3);
        assert_eq!(g.base_amount, 5.0);
        assert_eq!(g.max_net, 15.0);
    }

    #[test]
    fn month_from_test_drops_leftover() {
        let leftover = t() + Duration::days(2);
        let g = next_access(Sku::Month, t(), "test", Some(leftover));
        assert_eq!(g.plan, "month");
        assert_eq!((g.until - t()).num_days(), 30);
        assert_eq!(g.base_amount, 5.0);
        assert_eq!(g.max_net, 15.0);
    }

    #[test]
    fn month_renew_stacks_on_active_expiry() {
        let until = t() + Duration::days(10);
        let g = next_access(Sku::Month, t(), "month", Some(until));
        assert_eq!((g.until - until).num_days(), 30);
    }

    #[test]
    fn month_expired_starts_now() {
        let until = t() - Duration::days(1);
        let g = next_access(Sku::Month, t(), "month", Some(until));
        assert_eq!((g.until - t()).num_days(), 30);
    }

    #[test]
    fn expiry_drops_from_waves() {
        let now = 1_000_000;
        assert!(wave_eligible(now + 1, now, false, false));
        assert!(!wave_eligible(now, now, false, false));
        assert!(!wave_eligible(now + 1, now, true, false));
        assert!(!wave_eligible(now + 1, now, false, true));
    }

    #[test]
    fn start_paper_stops_live() {
        let f = next_run("paper", true).unwrap();
        assert!(f.paper_running());
        assert!(!f.live_running());
        assert_eq!(f.status(), "paper");
    }

    #[test]
    fn start_live_stops_paper() {
        let f = next_run("live", true).unwrap();
        assert!(f.live_running());
        assert!(!f.paper_running());
        assert_eq!(f.status(), "live");
    }

    #[test]
    fn stop_either_mode_stops_both() {
        assert_eq!(next_run("paper", false).unwrap(), RunFlags::stopped());
        assert_eq!(next_run("live", false).unwrap(), RunFlags::stopped());
        assert_eq!(RunFlags::stopped().status(), "stopped");
    }

    #[test]
    fn unpaid_is_not_vip() {
        let now = 1_000_000;
        assert!(!vip_open(false, 0, now, false));
        assert!(!vip_open(false, now, now, false));
        assert!(!vip_open(false, now + 1, now, true));
        assert!(vip_open(false, now + 1, now, false));
        assert!(vip_open(true, 0, now, false));
    }
}
