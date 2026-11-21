//! A time histogram that picks its own bucket size while streaming.
//!
//! Counting starts in one-second buckets. Whenever the covered time span
//! needs more than [`MAX_LIVE_BUCKETS`] buckets, neighbouring buckets are
//! merged into the next size on a fixed ladder (1s, 5s, 10s, 30s, 1m, …).
//! Every size on the ladder is a multiple of the one before it and buckets
//! are aligned to the epoch, so merging is exact: the result is the same as
//! if the final size had been used from the first line. Memory stays bounded
//! no matter how long the log runs.
//!
//! Burst detection works on the finished buckets: a bucket is part of a
//! burst when its error count is at least [`BURST_MIN_ERRORS`] and at least
//! [`BURST_FACTOR`] times the median error count per bucket.

use std::collections::HashMap;

/// Upper bound on buckets held while reading, in automatic mode.
pub const MAX_LIVE_BUCKETS: i64 = 4096;
/// Automatic mode finishes with at most this many buckets.
pub const TARGET_BUCKETS: i64 = 40;
/// A burst bucket has at least this many error lines.
pub const BURST_MIN_ERRORS: u64 = 5;
/// A burst bucket has at least this many times the median error count.
pub const BURST_FACTOR: u64 = 3;

const LADDER: [i64; 15] = [
    1, 5, 10, 30, 60, 300, 600, 1800, 3600, 10_800, 21_600, 43_200, 86_400, 604_800, 2_419_200,
];

/// One finished bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bucket {
    /// Epoch seconds of the start of the bucket.
    pub start: i64,
    pub count: u64,
    pub errors: u64,
}

/// A run of adjacent buckets with unusually many errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Burst {
    pub start: i64,
    /// Exclusive end: the start of the first bucket after the burst.
    pub end: i64,
    pub errors: u64,
    pub lines: u64,
    pub peak_errors: u64,
    /// Median errors per bucket over the whole time range.
    pub baseline_errors: u64,
}

#[derive(Debug)]
pub struct Histogram {
    size: i64,
    auto: bool,
    cells: HashMap<i64, (u64, u64)>,
    min: i64,
    max: i64,
}

fn next_size(size: i64) -> i64 {
    LADDER
        .iter()
        .copied()
        .find(|&s| s > size && s % size == 0)
        .unwrap_or(size * 2)
}

impl Histogram {
    /// `bucket` fixes the bucket size in seconds; `None` selects it
    /// automatically.
    pub fn new(bucket: Option<i64>) -> Self {
        Histogram {
            size: bucket.unwrap_or(1).max(1),
            auto: bucket.is_none(),
            cells: HashMap::new(),
            min: i64::MAX,
            max: i64::MIN,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    pub fn bucket_seconds(&self) -> i64 {
        self.size
    }

    /// Earliest and latest timestamps seen.
    pub fn range(&self) -> Option<(i64, i64)> {
        (!self.is_empty()).then_some((self.min, self.max))
    }

    fn span(&self) -> i64 {
        if self.is_empty() {
            0
        } else {
            self.max.div_euclid(self.size) - self.min.div_euclid(self.size) + 1
        }
    }

    pub fn add(&mut self, ts: i64, is_error: bool) {
        self.min = self.min.min(ts);
        self.max = self.max.max(ts);
        let cell = self.cells.entry(ts.div_euclid(self.size)).or_default();
        cell.0 += 1;
        cell.1 += u64::from(is_error);
        if self.auto && self.span() > MAX_LIVE_BUCKETS {
            self.coarsen_to(MAX_LIVE_BUCKETS / 2);
        }
    }

    fn coarsen_to(&mut self, max_buckets: i64) {
        let old = self.size;
        while self.span() > max_buckets {
            self.size = next_size(self.size);
        }
        if self.size == old {
            return;
        }
        let ratio = self.size / old;
        let mut merged: HashMap<i64, (u64, u64)> = HashMap::new();
        for (key, (count, errors)) in self.cells.drain() {
            let cell = merged.entry(key.div_euclid(ratio)).or_default();
            cell.0 += count;
            cell.1 += errors;
        }
        self.cells = merged;
    }

    /// Settles the final bucket size and returns the non-empty buckets in
    /// time order.
    pub fn finish(&mut self) -> Vec<Bucket> {
        if self.auto {
            self.coarsen_to(TARGET_BUCKETS);
        }
        let mut buckets: Vec<Bucket> = self
            .cells
            .iter()
            .map(|(&key, &(count, errors))| Bucket {
                start: key * self.size,
                count,
                errors,
            })
            .collect();
        buckets.sort_by_key(|b| b.start);
        buckets
    }
}

/// Finds error bursts in buckets returned by [`Histogram::finish`].
pub fn bursts(buckets: &[Bucket], size: i64) -> Vec<Burst> {
    let (Some(first), Some(last)) = (buckets.first(), buckets.last()) else {
        return Vec::new();
    };
    // Median over every bucket in the range, where the empty ones count as
    // zero errors.
    let span = ((last.start - first.start) / size + 1) as usize;
    let mut errors: Vec<u64> = buckets.iter().map(|b| b.errors).collect();
    errors.sort_unstable();
    let empty = span - errors.len();
    let mid = (span - 1) / 2;
    let median = if mid < empty { 0 } else { errors[mid - empty] };
    let threshold = BURST_MIN_ERRORS.max(BURST_FACTOR * median);

    let mut out: Vec<Burst> = Vec::new();
    for b in buckets.iter().filter(|b| b.errors >= threshold) {
        match out.last_mut() {
            Some(open) if open.end == b.start => {
                open.end = b.start + size;
                open.errors += b.errors;
                open.lines += b.count;
                open.peak_errors = open.peak_errors.max(b.errors);
            }
            _ => out.push(Burst {
                start: b.start,
                end: b.start + size,
                errors: b.errors,
                lines: b.count,
                peak_errors: b.errors,
                baseline_errors: median,
            }),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ladder_steps_divide_evenly() {
        for pair in LADDER.windows(2) {
            assert_eq!(pair[1] % pair[0], 0, "{pair:?}");
        }
        assert_eq!(next_size(1), 5);
        assert_eq!(next_size(60), 300);
        assert_eq!(next_size(2_419_200), 4_838_400);
        // A custom size moves to the next ladder entry it divides.
        assert_eq!(next_size(7), 604_800);
    }

    #[test]
    fn fixed_bucket_size() {
        let mut h = Histogram::new(Some(60));
        for (ts, err) in [(0, false), (59, true), (60, false), (200, false)] {
            h.add(ts, err);
        }
        assert_eq!(h.range(), Some((0, 200)));
        assert_eq!(
            h.finish(),
            vec![
                Bucket {
                    start: 0,
                    count: 2,
                    errors: 1
                },
                Bucket {
                    start: 60,
                    count: 1,
                    errors: 0
                },
                Bucket {
                    start: 180,
                    count: 1,
                    errors: 0
                },
            ]
        );
        assert_eq!(h.bucket_seconds(), 60);
    }

    #[test]
    fn automatic_size_for_one_hour() {
        let mut h = Histogram::new(None);
        for ts in 0..3600 {
            h.add(1_700_000_000 + ts, false);
        }
        let buckets = h.finish();
        assert_eq!(h.bucket_seconds(), 300);
        assert!(buckets.len() as i64 <= TARGET_BUCKETS);
        assert_eq!(buckets.iter().map(|b| b.count).sum::<u64>(), 3600);
    }

    #[test]
    fn automatic_matches_fixed_after_merging() {
        // Ten days of events at an odd stride, in a scrambled order, so the
        // automatic histogram has to merge several times along the way.
        let events: Vec<(i64, bool)> = (0..20_000)
            .map(|i| {
                let k = (i * 7919) % 20_000;
                (1_700_000_000 + k * 43, k % 9 == 0)
            })
            .collect();
        let mut auto = Histogram::new(None);
        for &(ts, err) in &events {
            auto.add(ts, err);
            assert!(auto.cells.len() as i64 <= MAX_LIVE_BUCKETS);
        }
        let auto_buckets = auto.finish();
        let mut fixed = Histogram::new(Some(auto.bucket_seconds()));
        for &(ts, err) in &events {
            fixed.add(ts, err);
        }
        assert_eq!(auto_buckets, fixed.finish());
        assert!(auto.bucket_seconds() > 1);
    }

    #[test]
    fn single_timestamp_and_empty() {
        let mut h = Histogram::new(None);
        assert!(h.finish().is_empty());
        assert_eq!(h.range(), None);
        h.add(42, true);
        assert_eq!(
            h.finish(),
            vec![Bucket {
                start: 42,
                count: 1,
                errors: 1
            }]
        );
    }

    #[test]
    fn negative_epochs_bucket_downwards() {
        let mut h = Histogram::new(Some(60));
        h.add(-1, false);
        assert_eq!(h.finish()[0].start, -60);
    }

    fn bucket(start: i64, count: u64, errors: u64) -> Bucket {
        Bucket {
            start,
            count,
            errors,
        }
    }

    #[test]
    fn burst_above_quiet_baseline() {
        let mut buckets: Vec<Bucket> = (0..10).map(|i| bucket(i * 60, 100, 1)).collect();
        buckets[4] = bucket(240, 300, 80);
        buckets[5] = bucket(300, 250, 40);
        assert_eq!(
            bursts(&buckets, 60),
            vec![Burst {
                start: 240,
                end: 360,
                errors: 120,
                lines: 550,
                peak_errors: 80,
                baseline_errors: 1,
            }]
        );
    }

    #[test]
    fn steady_errors_are_not_a_burst() {
        let buckets: Vec<Bucket> = (0..10)
            .map(|i| bucket(i * 60, 100, 50 + i as u64))
            .collect();
        assert!(bursts(&buckets, 60).is_empty());
    }

    #[test]
    fn few_errors_are_not_a_burst() {
        let mut buckets: Vec<Bucket> = (0..10).map(|i| bucket(i * 60, 100, 0)).collect();
        buckets[3].errors = BURST_MIN_ERRORS - 1;
        assert!(bursts(&buckets, 60).is_empty());
        buckets[3].errors = BURST_MIN_ERRORS;
        assert_eq!(bursts(&buckets, 60).len(), 1);
    }

    #[test]
    fn empty_buckets_lower_the_baseline_and_split_bursts() {
        // Errors at minutes 0 and 30 only; the 29 silent minutes between
        // them make the median zero.
        let buckets = vec![bucket(0, 20, 20), bucket(1800, 30, 30)];
        let found = bursts(&buckets, 60);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].baseline_errors, 0);
        assert_eq!((found[1].start, found[1].end), (1800, 1860));
        assert!(bursts(&[], 60).is_empty());
    }
}
