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
    pub p95: f64,
    pub p99: f64,
    pub min: f64,
    pub max: f64,
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
        p95: percentile(&sorted, 95.0),
        p99: percentile(&sorted, 99.0),
        min: sorted[0],
        max: sorted[n - 1],
        first: finite[0].0,
        last: finite[finite.len() - 1].0,
    })
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
    use super::{change_pct, percentile, stats};

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
    fn changes_are_relative_to_before() {
        assert_eq!(change_pct(200.0, 150.0), Some(-25.0));
        assert_eq!(change_pct(0.0, 1.0), None);
    }
}
