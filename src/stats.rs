//! Deterministic statistics for transfer samples.
//!
//! All functions reject empty input and non-finite values by returning `None`.
//! Percentiles use the Hyndman-Fan type 7 definition:
//! `sorted[(n - 1) * p / 100]` with linear interpolation between neighbors.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quartiles {
    pub q1: f64,
    pub q2: f64,
    pub q3: f64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TukeyOutlierCounts {
    pub mild: usize,
    pub severe: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TukeyFences {
    pub mild_lower: f64,
    pub mild_upper: f64,
    pub severe_lower: f64,
    pub severe_upper: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TukeyOutliers {
    pub counts: TukeyOutlierCounts,
    pub fences: TukeyFences,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Summary {
    pub count: usize,
    pub median: f64,
    pub mad: f64,
    pub p5: f64,
    pub p95: f64,
    pub quartiles: Quartiles,
    pub outliers: TukeyOutliers,
}

pub fn median(samples: &[f64]) -> Option<f64> {
    percentile(samples, 50.0)
}

pub fn mad(samples: &[f64]) -> Option<f64> {
    let center = median(samples)?;
    let deviations = samples
        .iter()
        .map(|sample| (sample - center).abs())
        .collect::<Vec<_>>();

    median(&deviations)
}

pub fn percentile(samples: &[f64], percentile: f64) -> Option<f64> {
    if !(0.0..=100.0).contains(&percentile) {
        return None;
    }

    let sorted = sorted_finite(samples)?;
    Some(percentile_sorted(&sorted, percentile))
}

pub fn quartiles(samples: &[f64]) -> Option<Quartiles> {
    let sorted = sorted_finite(samples)?;
    Some(Quartiles {
        q1: percentile_sorted(&sorted, 25.0),
        q2: percentile_sorted(&sorted, 50.0),
        q3: percentile_sorted(&sorted, 75.0),
    })
}

pub fn tukey_outliers(samples: &[f64]) -> Option<TukeyOutliers> {
    let quartiles = quartiles(samples)?;
    let iqr = quartiles.q3 - quartiles.q1;
    let fences = TukeyFences {
        mild_lower: quartiles.q1 - 1.5 * iqr,
        mild_upper: quartiles.q3 + 1.5 * iqr,
        severe_lower: quartiles.q1 - 3.0 * iqr,
        severe_upper: quartiles.q3 + 3.0 * iqr,
    };

    let mut counts = TukeyOutlierCounts { mild: 0, severe: 0 };
    for sample in samples {
        if !sample.is_finite() {
            return None;
        }

        if *sample < fences.severe_lower || *sample > fences.severe_upper {
            counts.severe += 1;
        } else if *sample < fences.mild_lower || *sample > fences.mild_upper {
            counts.mild += 1;
        }
    }

    Some(TukeyOutliers { counts, fences })
}

pub fn summarize(samples: &[f64]) -> Option<Summary> {
    Some(Summary {
        count: samples.len(),
        median: median(samples)?,
        mad: mad(samples)?,
        p5: percentile(samples, 5.0)?,
        p95: percentile(samples, 95.0)?,
        quartiles: quartiles(samples)?,
        outliers: tukey_outliers(samples)?,
    })
}

fn sorted_finite(samples: &[f64]) -> Option<Vec<f64>> {
    if samples.is_empty() || samples.iter().any(|sample| !sample.is_finite()) {
        return None;
    }

    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    Some(sorted)
}

fn percentile_sorted(sorted: &[f64], percentile: f64) -> f64 {
    debug_assert!(!sorted.is_empty());
    debug_assert!((0.0..=100.0).contains(&percentile));

    if sorted.len() == 1 {
        return sorted[0];
    }

    let rank = (percentile / 100.0) * (sorted.len() - 1) as f64;
    let lower_index = rank.floor() as usize;
    let upper_index = rank.ceil() as usize;

    if lower_index == upper_index {
        sorted[lower_index]
    } else {
        let weight = rank - lower_index as f64;
        sorted[lower_index] + (sorted[upper_index] - sorted[lower_index]) * weight
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPSILON: f64 = 1e-12;

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() <= EPSILON,
            "actual {actual}, expected {expected}"
        );
    }

    #[test]
    fn median_rejects_empty_and_non_finite_samples() {
        assert_eq!(median(&[]), None);
        assert_eq!(median(&[1.0, f64::NAN]), None);
        assert_eq!(median(&[1.0, f64::INFINITY]), None);
        assert_eq!(median(&[1.0, f64::NEG_INFINITY]), None);
    }

    #[test]
    fn median_handles_odd_even_and_unsorted_samples() {
        assert_close(median(&[9.0]).unwrap(), 9.0);
        assert_close(median(&[3.0, 1.0, 2.0]).unwrap(), 2.0);
        assert_close(median(&[10.0, 2.0, 4.0, 8.0]).unwrap(), 6.0);
    }

    #[test]
    fn percentile_uses_type_7_linear_interpolation() {
        let samples = [10.0, 20.0, 30.0, 40.0, 50.0];
        assert_close(percentile(&samples, 0.0).unwrap(), 10.0);
        assert_close(percentile(&samples, 5.0).unwrap(), 12.0);
        assert_close(percentile(&samples, 25.0).unwrap(), 20.0);
        assert_close(percentile(&samples, 50.0).unwrap(), 30.0);
        assert_close(percentile(&samples, 95.0).unwrap(), 48.0);
        assert_close(percentile(&samples, 100.0).unwrap(), 50.0);
    }

    #[test]
    fn percentile_rejects_out_of_range_percentiles() {
        assert_eq!(percentile(&[1.0, 2.0], -0.1), None);
        assert_eq!(percentile(&[1.0, 2.0], 100.1), None);
    }

    #[test]
    fn quartiles_are_deterministic_for_tiny_sample_sets() {
        assert_eq!(
            quartiles(&[8.0]).unwrap(),
            Quartiles {
                q1: 8.0,
                q2: 8.0,
                q3: 8.0
            }
        );

        assert_eq!(
            quartiles(&[1.0, 3.0]).unwrap(),
            Quartiles {
                q1: 1.5,
                q2: 2.0,
                q3: 2.5
            }
        );
    }

    #[test]
    fn quartiles_match_percentile_definition() {
        assert_eq!(
            quartiles(&[4.0, 1.0, 3.0, 2.0]).unwrap(),
            Quartiles {
                q1: 1.75,
                q2: 2.5,
                q3: 3.25
            }
        );
    }

    #[test]
    fn mad_is_median_of_absolute_deviations_from_median() {
        assert_close(mad(&[1.0, 1.0, 2.0, 2.0, 4.0, 6.0, 9.0]).unwrap(), 1.0);
        assert_close(mad(&[2.0, 4.0, 8.0, 10.0]).unwrap(), 3.0);
        assert_close(mad(&[5.0, 5.0, 5.0]).unwrap(), 0.0);
    }

    #[test]
    fn tukey_outliers_classify_mild_and_severe_counts() {
        let samples = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 20.0, 40.0];
        let outliers = tukey_outliers(&samples).unwrap();

        assert_eq!(
            outliers.fences,
            TukeyFences {
                mild_lower: -5.5,
                mild_upper: 16.5,
                severe_lower: -13.75,
                severe_upper: 24.75
            }
        );
        assert_eq!(outliers.counts, TukeyOutlierCounts { mild: 1, severe: 1 });
    }

    #[test]
    fn tukey_outliers_count_severe_separately_from_mild() {
        let samples = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 40.0];
        let outliers = tukey_outliers(&samples).unwrap();

        assert_eq!(outliers.counts, TukeyOutlierCounts { mild: 0, severe: 1 });
    }

    #[test]
    fn tukey_outliers_do_not_count_values_on_fences() {
        let samples = [0.0, 0.0, 0.0, 0.0];
        let outliers = tukey_outliers(&samples).unwrap();

        assert_eq!(
            outliers.fences,
            TukeyFences {
                mild_lower: 0.0,
                mild_upper: 0.0,
                severe_lower: 0.0,
                severe_upper: 0.0
            }
        );
        assert_eq!(outliers.counts, TukeyOutlierCounts { mild: 0, severe: 0 });
    }

    #[test]
    fn summarize_retains_core_report_statistics() {
        let summary = summarize(&[10.0, 20.0, 30.0, 40.0, 50.0]).unwrap();

        assert_eq!(summary.count, 5);
        assert_close(summary.median, 30.0);
        assert_close(summary.mad, 10.0);
        assert_close(summary.p5, 12.0);
        assert_close(summary.p95, 48.0);
        assert_eq!(
            summary.quartiles,
            Quartiles {
                q1: 20.0,
                q2: 30.0,
                q3: 40.0
            }
        );
        assert_eq!(
            summary.outliers.counts,
            TukeyOutlierCounts { mild: 0, severe: 0 }
        );
    }
}
