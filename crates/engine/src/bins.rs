/// Preallocated forward-filled bins. No growth on a BBA tick.

#[derive(Clone, Debug)]
pub struct Bins {
    step_ms: u64,
    values: Vec<f64>,
    last_write: i32,
    bin_start: u64,
}

impl Bins {
    pub fn new(bucket_ms: u64, step_ms: u64) -> Self {
        let n = (bucket_ms / step_ms) as usize;
        Self {
            step_ms,
            values: vec![0.0; n],
            last_write: -1,
            bin_start: 0,
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    #[inline]
    pub fn step_ms(&self) -> u64 {
        self.step_ms
    }

    #[inline]
    pub fn bin_start(&self) -> u64 {
        self.bin_start
    }

    pub fn roll(&mut self, bin_start: u64) {
        self.bin_start = bin_start;
        self.last_write = -1;
        self.values.fill(0.0);
    }

    /// Fill-forward `value` through `ts` (exchange time). Returns the written index.
    pub fn write(&mut self, ts: u64, value: f64) -> i32 {
        if ts < self.bin_start {
            return self.last_write;
        }
        let idx = ((ts - self.bin_start) / self.step_ms) as i32;
        let max_idx = self.values.len() as i32 - 1;
        let idx = idx.clamp(0, max_idx);
        let start = (self.last_write + 1).max(0);
        for i in start..=idx {
            self.values[i as usize] = value;
        }
        self.last_write = idx;
        idx
    }

    #[inline]
    pub fn get_at_ts(&self, ts: u64) -> Option<f64> {
        if ts < self.bin_start {
            return None;
        }
        let idx = ((ts - self.bin_start) / self.step_ms) as i32;
        self.get_idx(idx)
    }

    #[inline]
    pub fn get_idx(&self, idx: i32) -> Option<f64> {
        if idx < 0 || idx as usize >= self.values.len() {
            return None;
        }
        let v = self.values[idx as usize];
        if v > 0.0 {
            Some(v)
        } else {
            None
        }
    }

    /// Inclusive range of bins for `[ts_min, ts_max]`. All must be filled.
    pub fn range_minmax(&self, ts_min: u64, ts_max: u64) -> Option<(f64, f64, usize)> {
        let min_idx = ((ts_min.saturating_sub(self.bin_start)) / self.step_ms) as i32;
        let max_idx = ((ts_max.saturating_sub(self.bin_start)) / self.step_ms) as i32;
        if min_idx < 0 || max_idx < min_idx {
            return None;
        }
        let expected = (max_idx - min_idx + 1) as usize;
        let mut mn = f64::MAX;
        let mut mx = f64::MIN;
        for i in min_idx..=max_idx {
            let v = self.get_idx(i)?;
            mn = mn.min(v);
            mx = mx.max(v);
        }
        Some((mn, mx, expected))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_forward_and_no_growth() {
        let mut b = Bins::new(300_000, 100);
        assert_eq!(b.len(), 3000);
        b.roll(1_000_000);
        b.write(1_000_000, 10.0);
        b.write(1_000_350, 11.0);
        assert_eq!(b.get_idx(0), Some(10.0));
        assert_eq!(b.get_idx(1), Some(11.0));
        assert_eq!(b.get_idx(2), Some(11.0));
        assert_eq!(b.get_idx(3), Some(11.0));
        assert_eq!(b.get_idx(4), None);
        let cap = b.values.capacity();
        b.write(1_000_900, 12.0);
        assert_eq!(b.values.capacity(), cap);
    }

    #[test]
    fn range_requires_complete_window() {
        let mut b = Bins::new(1_000, 50);
        b.roll(0);
        b.write(0, 4.0);
        // first write only fills idx 0; later bins are empty
        assert!(b.range_minmax(0, 200).is_none());
        b.write(200, 6.0);
        let (mn, mx, n) = b.range_minmax(0, 200).unwrap();
        assert_eq!(n, 5);
        assert_eq!(mn, 4.0);
        assert_eq!(mx, 6.0);
        assert!(b.range_minmax(0, 400).is_none());
        assert!(b.range_minmax(200, 100).is_none(), "inverted window");
    }
}
