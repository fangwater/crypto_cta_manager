//! Online equivalent of the timeline's extrema downsampling. Interior points
//! are discarded as soon as their bucket extrema have been recorded.
use super::*;

const VALUES: [fn(&NavTimelinePoint) -> f64; 6] = [
    |p| p.totals.nav_change_before_fee_quote,
    |p| p.totals.nav_change_after_fee_quote,
    |p| p.totals.realized_pnl_before_fee_quote,
    |p| p.totals.floating_pnl_quote,
    |p| p.gross_position_value_quote,
    |p| p.net_position_value_quote,
];

#[derive(Default)]
pub(super) struct Samples {
    output: Vec<NavTimelinePoint>,
    pending: Option<NavTimelinePoint>,
    extrema: Option<[(NavTimelinePoint, NavTimelinePoint); 6]>,
    bucket_size: usize,
    bucket_used: usize,
    count: usize,
}
impl Samples {
    pub fn new(start: i64, end: i64, tick: i64, max: usize) -> Self {
        let interior = if end > start {
            ((end - 1).div_euclid(tick) - start.div_euclid(tick)).max(0) as usize
        } else {
            0
        };
        let total = interior + if start == end { 1 } else { 2 };
        let bucket_size = if total <= max || max < 10 {
            0
        } else {
            interior.div_ceil(((max - 2) / 12).max(1))
        };
        Self {
            bucket_size,
            ..Default::default()
        }
    }
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
    pub fn len(&self) -> usize {
        self.count
    }
    pub fn push(&mut self, p: NavTimelinePoint) {
        if self.pending.is_some_and(|old| old.ts_us == p.ts_us) {
            self.pending = Some(p);
            return;
        }
        if let Some(old) = self.pending.replace(p) {
            if self.count == 1 || self.bucket_size == 0 {
                self.output.push(old);
            } else {
                if let Some(extrema) = &mut self.extrema {
                    for (i, (min, max)) in extrema.iter_mut().enumerate() {
                        // Iterator::min_by keeps the first tie; max_by the last.
                        if VALUES[i](&old).partial_cmp(&VALUES[i](min)) == Some(Ordering::Less) {
                            *min = old;
                        }
                        if VALUES[i](&old).partial_cmp(&VALUES[i](max)) != Some(Ordering::Less) {
                            *max = old;
                        }
                    }
                } else {
                    self.extrema = Some([(old, old); 6]);
                }
                self.bucket_used += 1;
                if self.bucket_used == self.bucket_size {
                    self.flush();
                }
            }
        }
        self.count += 1;
    }
    fn flush(&mut self) {
        if let Some(extrema) = self.extrema.take() {
            let mut values = extrema
                .into_iter()
                .flat_map(|(a, b)| [a, b])
                .collect::<Vec<_>>();
            values.sort_by_key(|p| p.ts_us);
            values.dedup_by_key(|p| p.ts_us);
            self.output.extend(values);
        }
        self.bucket_used = 0;
    }
    pub fn finish(mut self) -> Vec<NavTimelinePoint> {
        self.flush();
        if let Some(last) = self.pending {
            self.output.push(last);
        }
        self.output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn online_extrema_match_full_series_including_replaced_endpoints() {
        for length in [1, 2, 99, 101, 801, 3501, 9000] {
            for max in [100, 800, 3500] {
                let mut samples = Samples::new(0, length - 1, 1, max);
                let mut raw = Vec::new();
                for i in 0..length {
                    let p = NavTimelinePoint {
                        ts_us: i,
                        totals: NavTotals {
                            nav_change_before_fee_quote: ((i * 17) % 29) as f64,
                            nav_change_after_fee_quote: ((i * 13) % 31) as f64,
                            ..Default::default()
                        },
                        ..Default::default()
                    };
                    samples.push(p);
                    raw.push(p);
                }
                let mut last = *raw.last().unwrap();
                last.totals.realized_pnl_before_fee_quote = 42.0;
                *raw.last_mut().unwrap() = last;
                samples.push(last);
                assert_eq!(samples.finish(), downsample_timeline_points(raw, max));
            }
        }
    }
}
