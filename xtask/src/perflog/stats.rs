//! Order statistics over samples.

/// What a set of values gives. Percentiles interpolate linearly between
/// the two nearest ranks (as NumPy's default does).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stats {
    pub n: usize,
    pub mean: f64,
    /// Mean weighted by each sample's frames, where samples carry them.
    pub weighted_mean: Option<f64>,
    pub median: f64,
    /// The first and third quartiles: `p75 - p25` is the interquartile
    /// range, the spread of the middle half.
    pub p25: f64,
    pub p75: f64,
    pub p95: f64,
    pub p99: f64,
    pub min: f64,
    pub max: f64,
    /// Standard deviation (population).
    pub stdev: f64,
    /// The first and last value in log order.
    pub first: f64,
    pub last: f64,
}

/// The `p`-th percentile (0 to 100) of sorted, finite `values`.
pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    match sorted.len() {
        0 => f64::NAN,
        1 => sorted[0],
        n => {
            let rank = (p / 100.0).clamp(0.0, 1.0) * (n - 1) as f64;
            let lo = rank.floor() as usize;
            let hi = rank.ceil() as usize;
            sorted[lo] + (sorted[hi] - sorted[lo]) * (rank - lo as f64)
        }
    }
}

/// Statistics of `(value, weight)` pairs in log order; non-finite values
/// are left out. `None` when nothing is left. Weights of zero or less mean
/// "no weight": the weighted mean is given only when every value has one.
pub fn stats(values: &[(f64, f64)]) -> Option<Stats> {
    let finite: Vec<(f64, f64)> = values
        .iter()
        .copied()
        .filter(|(v, _)| v.is_finite())
        .collect();
    if finite.is_empty() {
        return None;
    }
    let mut sorted: Vec<f64> = finite.iter().map(|(v, _)| *v).collect();
    sorted.sort_by(f64::total_cmp);
    let n = sorted.len();
    let mean = sorted.iter().sum::<f64>() / n as f64;
    let stdev = (sorted.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n as f64).sqrt();
    let weighted_mean = if finite.iter().all(|(_, w)| *w > 0.0) {
        let total: f64 = finite.iter().map(|(_, w)| w).sum();
        Some(finite.iter().map(|(v, w)| v * w).sum::<f64>() / total)
    } else {
        None
    };
    Some(Stats {
        n,
        mean,
        weighted_mean,
        median: percentile(&sorted, 50.0),
        p25: percentile(&sorted, 25.0),
        p75: percentile(&sorted, 75.0),
        p95: percentile(&sorted, 95.0),
        p99: percentile(&sorted, 99.0),
        min: sorted[0],
        max: sorted[n - 1],
        stdev,
        first: finite[0].0,
        last: finite[finite.len() - 1].0,
    })
}

impl Stats {
    /// The interquartile range.
    pub fn iqr(&self) -> f64 {
        self.p75 - self.p25
    }

    /// The coefficient of variation (standard deviation over mean), when
    /// the mean is not zero.
    pub fn cv(&self) -> Option<f64> {
        (self.mean.abs() > f64::EPSILON).then(|| self.stdev / self.mean.abs())
    }
}

/// Pearson's correlation of paired values; `None` with fewer than three
/// pairs or no spread.
pub fn correlation(pairs: &[(f64, f64)]) -> Option<f64> {
    let pairs: Vec<(f64, f64)> = pairs
        .iter()
        .copied()
        .filter(|(a, b)| a.is_finite() && b.is_finite())
        .collect();
    if pairs.len() < 3 {
        return None;
    }
    let n = pairs.len() as f64;
    let (ma, mb) = (
        pairs.iter().map(|p| p.0).sum::<f64>() / n,
        pairs.iter().map(|p| p.1).sum::<f64>() / n,
    );
    let (mut sab, mut saa, mut sbb) = (0.0, 0.0, 0.0);
    for (a, b) in pairs {
        sab += (a - ma) * (b - mb);
        saa += (a - ma).powi(2);
        sbb += (b - mb).powi(2);
    }
    (saa > 0.0 && sbb > 0.0).then(|| sab / (saa * sbb).sqrt())
}

/// The relative change from `before` to `after`, in percent.
pub fn change_pct(before: f64, after: f64) -> Option<f64> {
    if before.is_finite() && after.is_finite() && before.abs() > f64::EPSILON {
        Some(100.0 * (after - before) / before.abs())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{change_pct, correlation, percentile, stats};

    #[test]
    fn percentiles_interpolate_between_ranks() {
        let v: Vec<f64> = (1..=10).map(f64::from).collect();
        assert_eq!(percentile(&v, 50.0), 5.5);
        assert!((percentile(&v, 95.0) - 9.55).abs() < 1e-12);
        assert_eq!(percentile(&v, 0.0), 1.0);
        assert_eq!(percentile(&v, 100.0), 10.0);
        assert_eq!(percentile(&[7.0], 95.0), 7.0);
        assert!(percentile(&[], 50.0).is_nan());
    }

    #[test]
    fn stats_keep_log_order_for_first_and_last() {
        let s = stats(&[(3.0, 1.0), (1.0, 1.0), (f64::NAN, 1.0), (2.0, 1.0)]).unwrap();
        assert_eq!(s.n, 3);
        assert_eq!(
            (s.first, s.last, s.min, s.max, s.median),
            (3.0, 2.0, 1.0, 3.0, 2.0)
        );
        assert_eq!(s.mean, 2.0);
    }

    /// A window of 300 frames weighs ten times one of 30.
    #[test]
    fn weighted_means_use_frames() {
        let s = stats(&[(10.0, 300.0), (100.0, 30.0)]).unwrap();
        assert!((s.weighted_mean.unwrap() - (3000.0 + 3000.0) / 330.0).abs() < 1e-9);
        assert_eq!(
            stats(&[(1.0, 0.0), (2.0, 5.0)]).unwrap().weighted_mean,
            None
        );
        assert!(stats(&[]).is_none());
    }

    #[test]
    fn dispersion_and_correlation() {
        let s = stats(&[(1.0, 0.0), (2.0, 0.0), (3.0, 0.0), (4.0, 0.0), (5.0, 0.0)]).unwrap();
        assert_eq!((s.p25, s.p75, s.iqr()), (2.0, 4.0, 2.0));
        assert!((s.stdev - 2f64.sqrt()).abs() < 1e-12);
        assert!((s.cv().unwrap() - 2f64.sqrt() / 3.0).abs() < 1e-12);
        let up: Vec<(f64, f64)> = (0..10)
            .map(|i| (f64::from(i), 2.0 * f64::from(i) + 1.0))
            .collect();
        assert!((correlation(&up).unwrap() - 1.0).abs() < 1e-12);
        let down: Vec<(f64, f64)> = (0..10).map(|i| (f64::from(i), -f64::from(i))).collect();
        assert!((correlation(&down).unwrap() + 1.0).abs() < 1e-12);
        assert_eq!(correlation(&[(1.0, 1.0), (2.0, 1.0), (3.0, 1.0)]), None);
        assert_eq!(correlation(&[(1.0, 1.0), (2.0, 2.0)]), None);
    }

    #[test]
    fn changes_are_relative_to_before() {
        assert_eq!(change_pct(200.0, 150.0), Some(-25.0));
        assert_eq!(change_pct(0.0, 1.0), None);
    }
}
