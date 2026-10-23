//! Good-till-revoked bands: one per CLOB BBO change, valid for `gtr_ms` after the quote was last seen.

const CAP: usize = 64;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Band {
    /// YES book mid when the band was cut.
    pub mid: f64,
    /// min / max Ŝ over the window the book could have been quoted from.
    pub ref_min: f64,
    pub ref_max: f64,
    pub likely_ref: f64,
    pub nd2_scaled_min: f64,
    pub nd2_scaled_max: f64,
    pub expires_ms: u64,
}

#[derive(Clone, Debug)]
pub struct Bands {
    ring: [Band; CAP],
    head: usize,
    len: usize,
}

impl Default for Bands {
    fn default() -> Self {
        Self {
            ring: [Band::default(); CAP],
            head: 0,
            len: 0,
        }
    }
}

impl Bands {
    pub fn clear(&mut self) {
        self.len = 0;
        self.head = 0;
    }

    pub fn push(&mut self, band: Band) {
        if self.len < CAP {
            self.ring[(self.head + self.len) % CAP] = band;
            self.len += 1;
        } else {
            self.ring[self.head] = band;
            self.head = (self.head + 1) % CAP;
        }
    }

    pub fn extend_newest(&mut self, expires_ms: u64) {
        if self.len > 0 {
            let i = (self.head + self.len - 1) % CAP;
            self.ring[i].expires_ms = self.ring[i].expires_ms.max(expires_ms);
        }
    }

    fn nth_newest(&self, k: usize) -> Band {
        self.ring[(self.head + self.len - 1 - k) % CAP]
    }

    pub fn newest(&self) -> Option<Band> {
        (self.len > 0).then(|| self.nth_newest(0))
    }

    /// Unexpired bands, newest first, stopping at the first expired one.
    pub fn valid(&self, now_ms: u64, dst: &mut [Band; CAP]) -> usize {
        let mut n = 0;
        for k in 0..self.len {
            let b = self.nth_newest(k);
            if now_ms >= b.expires_ms {
                break;
            }
            dst[n] = b;
            n += 1;
        }
        n
    }

    pub fn max_nd2(&self) -> f64 {
        (0..self.len).map(|k| self.nth_newest(k).nd2_scaled_max).fold(0.0, f64::max)
    }

    pub const CAP: usize = CAP;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walk_back_stops_at_expired() {
        let mut b = Bands::default();
        for (i, exp) in [100u64, 50, 200, 300].iter().enumerate() {
            b.push(Band { mid: i as f64, expires_ms: *exp, ..Band::default() });
        }
        let mut buf = [Band::default(); CAP];
        let n = b.valid(60, &mut buf);
        assert_eq!(n, 2);
        assert_eq!(buf[0].mid, 3.0);
        assert_eq!(buf[1].mid, 2.0);
    }

    #[test]
    fn ring_overwrites_oldest() {
        let mut b = Bands::default();
        for i in 0..(CAP + 3) {
            b.push(Band { mid: i as f64, expires_ms: u64::MAX, ..Band::default() });
        }
        assert_eq!(b.newest().unwrap().mid, (CAP + 2) as f64);
        let mut buf = [Band::default(); CAP];
        assert_eq!(b.valid(0, &mut buf), CAP);
        assert_eq!(buf[CAP - 1].mid, 3.0);
    }
}
