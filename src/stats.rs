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
pub enum DistributionShape {
    Ordinary,
    SeparatedClusters(SeparatedClusters),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SeparatedClusters {
    pub lower_center: f64,
    pub lower_count: usize,
    pub upper_center: f64,
    pub upper_count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Summary {
    pub count: usize,
    pub median: f64,
    pub median_confidence: ConfidenceInterval,
    pub mad: f64,
    pub p5: f64,
    pub p95: f64,
    pub quartiles: Quartiles,
    pub outliers: TukeyOutliers,
    pub shape: DistributionShape,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ConfidenceInterval {
    pub confidence_level: f64,
    pub lower_bound: f64,
    pub upper_bound: f64,
    pub resamples: usize,
}

pub const CONFIDENCE_LEVEL: f64 = 0.95;
pub const BOOTSTRAP_RESAMPLES: usize = 10_000;
const MIN_SHAPE_SAMPLES: usize = 12;
const MIN_CLUSTER_COUNT: usize = 4;
const MIN_CLUSTER_FRACTION_NUMERATOR: usize = 15;
const MIN_CLUSTER_FRACTION_DENOMINATOR: usize = 100;
const MIN_CENTER_TO_WITHIN_MAD: f64 = 2.5;
const MIN_GAP_TO_OVERALL_SPREAD: f64 = 0.12;
const MIN_LARGEST_GAP_DOMINANCE: f64 = 1.25;

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

pub fn distribution_shape(samples: &[f64]) -> Option<DistributionShape> {
    let sorted = sorted_finite(samples)?;
    Some(distribution_shape_sorted(&sorted))
}

pub fn summarize(samples: &[f64]) -> Option<Summary> {
    let median = median(samples)?;
    let (lower_bound, upper_bound) = crate::bootstrap::median_confidence_interval(
        samples,
        CONFIDENCE_LEVEL,
        BOOTSTRAP_RESAMPLES,
    )?;

    Some(Summary {
        count: samples.len(),
        median,
        median_confidence: ConfidenceInterval {
            confidence_level: CONFIDENCE_LEVEL,
            lower_bound,
            upper_bound,
            resamples: BOOTSTRAP_RESAMPLES,
        },
        mad: mad(samples)?,
        p5: percentile(samples, 5.0)?,
        p95: percentile(samples, 95.0)?,
        quartiles: quartiles(samples)?,
        outliers: tukey_outliers(samples)?,
        shape: distribution_shape(samples)?,
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

fn distribution_shape_sorted(sorted: &[f64]) -> DistributionShape {
    if sorted.len() < MIN_SHAPE_SAMPLES {
        return DistributionShape::Ordinary;
    }

    let min_cluster_count = minimum_cluster_count(sorted.len());
    if min_cluster_count * 2 > sorted.len() {
        return DistributionShape::Ordinary;
    }

    let overall_spread = percentile_sorted(sorted, 95.0) - percentile_sorted(sorted, 5.0);
    if overall_spread <= 0.0 {
        return DistributionShape::Ordinary;
    }

    let Some((split, gap, next_largest_gap)) = largest_eligible_gap(sorted, min_cluster_count)
    else {
        return DistributionShape::Ordinary;
    };

    if next_largest_gap > 0.0 && gap < MIN_LARGEST_GAP_DOMINANCE * next_largest_gap {
        return DistributionShape::Ordinary;
    }

    let lower = &sorted[..split];
    let upper = &sorted[split..];
    let lower_center = median_sorted(lower);
    let upper_center = median_sorted(upper);
    let within_spread = mad_sorted(lower, lower_center).max(mad_sorted(upper, upper_center));
    let required_gap = MIN_GAP_TO_OVERALL_SPREAD * overall_spread;
    let center_separation = upper_center - lower_center;

    if gap < required_gap
        || center_separation < MIN_CENTER_TO_WITHIN_MAD * within_spread.max(f64::EPSILON)
    {
        return DistributionShape::Ordinary;
    }

    DistributionShape::SeparatedClusters(SeparatedClusters {
        lower_center,
        lower_count: lower.len(),
        upper_center,
        upper_count: upper.len(),
    })
}

fn minimum_cluster_count(sample_count: usize) -> usize {
    MIN_CLUSTER_COUNT.max(
        (sample_count * MIN_CLUSTER_FRACTION_NUMERATOR).div_ceil(MIN_CLUSTER_FRACTION_DENOMINATOR),
    )
}

fn largest_eligible_gap(sorted: &[f64], min_cluster_count: usize) -> Option<(usize, f64, f64)> {
    let mut largest = None;
    let mut next_largest_gap = 0.0;

    for split in min_cluster_count..=sorted.len() - min_cluster_count {
        let gap = sorted[split] - sorted[split - 1];
        if gap <= 0.0 {
            continue;
        }

        match largest {
            Some((_, largest_gap)) if gap > largest_gap => {
                next_largest_gap = largest_gap.max(next_largest_gap);
                largest = Some((split, gap));
            }
            Some(_) => {
                next_largest_gap = gap.max(next_largest_gap);
            }
            None => {
                largest = Some((split, gap));
            }
        }
    }

    largest.map(|(split, gap)| (split, gap, next_largest_gap))
}

fn median_sorted(sorted: &[f64]) -> f64 {
    percentile_sorted(sorted, 50.0)
}

fn mad_sorted(sorted: &[f64], center: f64) -> f64 {
    let mut deviations = sorted
        .iter()
        .map(|sample| (sample - center).abs())
        .collect::<Vec<_>>();
    deviations.sort_by(f64::total_cmp);
    median_sorted(&deviations)
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
    fn distribution_shape_rejects_invalid_input() {
        assert_eq!(distribution_shape(&[]), None);
        assert_eq!(distribution_shape(&[1.0, f64::NAN]), None);
        assert_eq!(distribution_shape(&[1.0, f64::INFINITY]), None);
        assert_eq!(distribution_shape(&[1.0, f64::NEG_INFINITY]), None);
    }

    #[test]
    fn distribution_shape_treats_identical_samples_as_ordinary() {
        let samples = [42.0; 20];

        assert_eq!(
            distribution_shape(&samples),
            Some(DistributionShape::Ordinary)
        );
    }

    #[test]
    fn distribution_shape_treats_evenly_spaced_unimodal_samples_as_ordinary() {
        let samples = [
            10.0, 10.1, 10.2, 10.3, 10.4, 10.5, 10.6, 10.7, 10.8, 10.9, 11.0, 11.1, 11.2, 11.3,
            11.4, 11.5, 11.6, 11.7, 11.8, 11.9,
        ];

        assert_eq!(
            distribution_shape(&samples),
            Some(DistributionShape::Ordinary)
        );
    }

    #[test]
    fn distribution_shape_detects_obvious_balanced_separated_clusters() {
        let samples = [
            40.0, 20.0, 20.0, 40.0, 20.0, 40.0, 20.0, 40.0, 20.0, 40.0, 20.0, 40.0, 20.0, 40.0,
            20.0, 40.0, 20.0, 40.0, 20.0, 40.0,
        ];

        assert_eq!(
            distribution_shape(&samples),
            Some(DistributionShape::SeparatedClusters(SeparatedClusters {
                lower_center: 20.0,
                lower_count: 10,
                upper_center: 40.0,
                upper_count: 10,
            }))
        );
    }

    #[test]
    fn distribution_shape_detects_unequal_but_substantial_clusters() {
        let samples = [
            100.0, 100.1, 100.2, 100.3, 100.4, 100.5, 100.6, 100.7, 100.8, 100.9, 101.0, 101.1,
            101.2, 101.3, 101.4, 101.5, 140.0, 140.1, 140.2, 140.3,
        ];

        let Some(DistributionShape::SeparatedClusters(clusters)) = distribution_shape(&samples)
        else {
            panic!("expected separated clusters");
        };

        assert_close(clusters.lower_center, 100.75);
        assert_eq!(clusters.lower_count, 16);
        assert_close(clusters.upper_center, 140.15);
        assert_eq!(clusters.upper_count, 4);
    }

    #[test]
    fn distribution_shape_detects_broad_lower_and_tight_upper_modes() {
        let samples = [
            28.5, 29.4, 30.2, 31.1, 32.4, 34.0, 35.1, 36.5, 37.6, 38.7, 41.4, 41.7, 42.0, 42.2,
            42.5, 42.7, 43.0, 43.2, 43.5, 43.9,
        ];

        let Some(DistributionShape::SeparatedClusters(clusters)) = distribution_shape(&samples)
        else {
            panic!("expected separated clusters");
        };
        assert_close(clusters.lower_center, 33.2);
        assert_eq!(clusters.lower_count, 10);
        assert_close(clusters.upper_center, 42.6);
        assert_eq!(clusters.upper_count, 10);
    }

    #[test]
    fn distribution_shape_rejects_one_or_two_isolated_outliers() {
        let one_outlier = [
            50.0, 50.0, 50.0, 50.0, 50.0, 50.0, 50.0, 50.0, 50.0, 50.0, 50.0, 50.0, 50.0, 50.0,
            50.0, 50.0, 50.0, 50.0, 50.0, 90.0,
        ];
        let two_outliers = [
            50.0, 50.0, 50.0, 50.0, 50.0, 50.0, 50.0, 50.0, 50.0, 50.0, 50.0, 50.0, 50.0, 50.0,
            50.0, 50.0, 50.0, 50.0, 90.0, 100.0,
        ];

        assert_eq!(
            distribution_shape(&one_outlier),
            Some(DistributionShape::Ordinary)
        );
        assert_eq!(
            distribution_shape(&two_outliers),
            Some(DistributionShape::Ordinary)
        );
    }

    #[test]
    fn distribution_shape_rejects_small_sample_sets() {
        let samples = [10.0, 10.0, 10.0, 10.0, 10.0, 30.0, 30.0, 30.0, 30.0, 30.0];

        assert_eq!(
            distribution_shape(&samples),
            Some(DistributionShape::Ordinary)
        );
    }

    #[test]
    fn summarize_retains_core_report_statistics() {
        let summary = summarize(&[10.0, 20.0, 30.0, 40.0, 50.0]).unwrap();

        assert_eq!(summary.count, 5);
        assert_close(summary.median, 30.0);
        assert_eq!(summary.median_confidence.confidence_level, 0.95);
        assert!(summary.median_confidence.lower_bound <= summary.median);
        assert!(summary.median_confidence.upper_bound >= summary.median);
        assert_eq!(summary.median_confidence.resamples, BOOTSTRAP_RESAMPLES);
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
        assert_eq!(summary.shape, DistributionShape::Ordinary);
    }
}
