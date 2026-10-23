use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Dual credit-pool limiter (lecture 8): burst window + 10-minute sustained.
/// CLOB queues instead of 429s when credits remain.
#[derive(Debug)]
pub struct DualLimiter {
    burst: Pool,
    sustained: Pool,
}

#[derive(Debug)]
struct Pool {
    credits: f64,
    cap: f64,
    refunds: VecDeque<(Instant, f64)>,
}

impl Pool {
    fn new(cap: f64) -> Self {
        Self {
            credits: cap,
            cap,
            refunds: VecDeque::with_capacity(1024),
        }
    }

    fn harvest(&mut self, now: Instant) {
        while let Some(&(t, c)) = self.refunds.front() {
            if t <= now {
                self.credits = (self.credits + c).min(self.cap);
                self.refunds.pop_front();
            } else {
                break;
            }
        }
    }

    fn try_take(&mut self, now: Instant, cost: f64, refund: Duration) -> bool {
        self.harvest(now);
        if self.credits < cost {
            return false;
        }
        self.credits -= cost;
        self.refunds.push_back((now + refund, cost));
        true
    }
}

impl DualLimiter {
    /// Defaults: 480 / 10s burst (lecture 500/10s with headroom) and 3500 / 10 min sustained.
    pub fn new(burst: f64, sustained: f64) -> Self {
        Self {
            burst: Pool::new(burst),
            sustained: Pool::new(sustained),
        }
    }

    pub fn production() -> Self {
        Self::new(480.0, 3500.0)
    }

    pub fn try_acquire(&mut self, cost: f64) -> bool {
        let now = Instant::now();
        self.burst.harvest(now);
        self.sustained.harvest(now);
        if self.burst.credits < cost || self.sustained.credits < cost {
            return false;
        }
        let ok_b = self.burst.try_take(now, cost, Duration::from_secs(10));
        let ok_s = self.sustained.try_take(now, cost, Duration::from_secs(600));
        debug_assert!(ok_b && ok_s);
        true
    }

    pub fn burst_credits(&self) -> f64 {
        self.burst.credits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn burst_exhausts() {
        let mut l = DualLimiter::new(2.0, 100.0);
        assert!(l.try_acquire(1.0));
        assert!(l.try_acquire(1.0));
        assert!(!l.try_acquire(1.0));
    }
}
