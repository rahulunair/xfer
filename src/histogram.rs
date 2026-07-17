//! Histogram binning and fixed-width ASCII rendering data.
//!
//! Empty input, zero bin requests, zero bar width, and non-finite samples are
//! rejected by returning `None`.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HistogramBin {
    pub lower: f64,
    pub upper: f64,
    pub count: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Histogram {
    pub sample_count: usize,
    pub lower: f64,
    pub upper: f64,
    pub bin_width: f64,
    pub bins: Vec<HistogramBin>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistogramRow {
    pub label: String,
    pub bar: String,
    pub count: usize,
    pub marks_median: bool,
}

impl Histogram {
    pub fn from_samples(samples: &[f64], requested_bins: usize) -> Option<Self> {
        if samples.is_empty()
            || requested_bins == 0
            || samples.iter().any(|sample| !sample.is_finite())
        {
            return None;
        }

        let lower = samples
            .iter()
            .copied()
            .min_by(f64::total_cmp)
            .expect("non-empty samples have a minimum");
        let upper = samples
            .iter()
            .copied()
            .max_by(f64::total_cmp)
            .expect("non-empty samples have a maximum");

        let bin_count = if lower == upper { 1 } else { requested_bins };
        let bin_width = if lower == upper {
            1.0
        } else {
            (upper - lower) / bin_count as f64
        };

        let mut bins = (0..bin_count)
            .map(|index| {
                let bin_lower = if lower == upper {
                    lower
                } else {
                    lower + index as f64 * bin_width
                };
                let bin_upper = if lower == upper || index + 1 == bin_count {
                    upper
                } else {
                    lower + (index + 1) as f64 * bin_width
                };

                HistogramBin {
                    lower: bin_lower,
                    upper: bin_upper,
                    count: 0,
                }
            })
            .collect::<Vec<_>>();

        for sample in samples {
            let index = bin_index(*sample, lower, upper, bin_width, bin_count);
            bins[index].count += 1;
        }

        Some(Self {
            sample_count: samples.len(),
            lower,
            upper,
            bin_width,
            bins,
        })
    }

    pub fn rows(&self, max_bar_width: usize, median: Option<f64>) -> Option<Vec<HistogramRow>> {
        if max_bar_width == 0 || self.bins.is_empty() {
            return None;
        }

        let max_count = self.bins.iter().map(|bin| bin.count).max().unwrap_or(0);
        let label_decimals = label_decimals(&self.bins);
        let labels = self
            .bins
            .iter()
            .map(|bin| format_label(bin.lower, label_decimals))
            .collect::<Vec<_>>();
        let label_width = labels.iter().map(String::len).max().unwrap_or(0);

        Some(
            self.bins
                .iter()
                .zip(labels)
                .map(|(bin, label)| {
                    let bar_len = scaled_bar_len(bin.count, max_count, max_bar_width);
                    let marks_median = median
                        .filter(|value| value.is_finite())
                        .is_some_and(|value| bin_contains(value, bin, self.upper));

                    HistogramRow {
                        label: format!("{label:>label_width$}"),
                        bar: "#".repeat(bar_len),
                        count: bin.count,
                        marks_median,
                    }
                })
                .collect(),
        )
    }

    pub fn render_ascii(&self, max_bar_width: usize, median: Option<f64>) -> Option<Vec<String>> {
        Some(
            self.rows(max_bar_width, median)?
                .into_iter()
                .map(|row| {
                    if row.marks_median {
                        format!("{} | {}  median", row.label, row.bar)
                    } else {
                        format!("{} | {}", row.label, row.bar)
                    }
                })
                .collect(),
        )
    }
}

pub fn bins(samples: &[f64], requested_bins: usize) -> Option<Vec<HistogramBin>> {
    Histogram::from_samples(samples, requested_bins).map(|histogram| histogram.bins)
}

fn bin_index(sample: f64, lower: f64, upper: f64, bin_width: f64, bin_count: usize) -> usize {
    if lower == upper || sample == upper {
        return bin_count - 1;
    }

    (((sample - lower) / bin_width).floor() as usize).min(bin_count - 1)
}

fn bin_contains(value: f64, bin: &HistogramBin, histogram_upper: f64) -> bool {
    if bin.lower == bin.upper {
        return value == bin.lower;
    }

    bin.lower <= value && (value < bin.upper || (value == histogram_upper && value == bin.upper))
}

fn label_decimals(bins: &[HistogramBin]) -> usize {
    if bins.len() <= 1 {
        return 1;
    }

    for decimals in 1..=6 {
        let mut labels = bins.iter().map(|bin| format_label(bin.lower, decimals));
        let Some(mut previous) = labels.next() else {
            return 1;
        };
        let mut distinct = true;
        for label in labels {
            if label == previous {
                distinct = false;
                break;
            }
            previous = label;
        }
        if distinct {
            return decimals;
        }
    }

    9
}

fn format_label(value: f64, decimals: usize) -> String {
    let rounded = format!("{value:.decimals$}");
    if rounded.starts_with('-') && rounded.parse::<f64>() == Ok(0.0) {
        rounded[1..].to_owned()
    } else {
        rounded
    }
}

fn scaled_bar_len(count: usize, max_count: usize, max_bar_width: usize) -> usize {
    if count == 0 || max_count == 0 {
        0
    } else {
        (count * max_bar_width).div_ceil(max_count)
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
    fn histogram_rejects_empty_zero_bins_and_non_finite_samples() {
        assert_eq!(Histogram::from_samples(&[], 4), None);
        assert_eq!(Histogram::from_samples(&[1.0], 0), None);
        assert_eq!(Histogram::from_samples(&[1.0, f64::NAN], 4), None);
        assert_eq!(Histogram::from_samples(&[1.0, f64::INFINITY], 4), None);
    }

    #[test]
    fn histogram_bins_sorted_range_into_half_open_bins() {
        let histogram = Histogram::from_samples(&[0.0, 0.9, 1.0, 1.9, 2.0, 2.9, 3.0], 3).unwrap();

        assert_eq!(histogram.sample_count, 7);
        assert_close(histogram.lower, 0.0);
        assert_close(histogram.upper, 3.0);
        assert_close(histogram.bin_width, 1.0);
        assert_eq!(
            histogram.bins,
            vec![
                HistogramBin {
                    lower: 0.0,
                    upper: 1.0,
                    count: 2
                },
                HistogramBin {
                    lower: 1.0,
                    upper: 2.0,
                    count: 2
                },
                HistogramBin {
                    lower: 2.0,
                    upper: 3.0,
                    count: 3
                }
            ]
        );
    }

    #[test]
    fn histogram_honors_requested_bins_for_tiny_sample_sets() {
        let histogram = Histogram::from_samples(&[10.0, 20.0], 10).unwrap();

        assert_eq!(histogram.bins.len(), 10);
        assert_eq!(histogram.bins[0].count, 1);
        assert_eq!(histogram.bins[9].count, 1);
        assert!(histogram.bins[1..9].iter().all(|bin| bin.count == 0));
    }

    #[test]
    fn histogram_handles_identical_samples_with_one_stable_bucket() {
        let histogram = Histogram::from_samples(&[7.0, 7.0, 7.0, 7.0], 6).unwrap();

        assert_eq!(histogram.sample_count, 4);
        assert_close(histogram.lower, 7.0);
        assert_close(histogram.upper, 7.0);
        assert_close(histogram.bin_width, 1.0);
        assert_eq!(
            histogram.bins,
            vec![HistogramBin {
                lower: 7.0,
                upper: 7.0,
                count: 4
            }]
        );
    }

    #[test]
    fn histogram_handles_single_sample() {
        let histogram = Histogram::from_samples(&[42.0], 8).unwrap();

        assert_eq!(
            histogram.bins,
            vec![HistogramBin {
                lower: 42.0,
                upper: 42.0,
                count: 1
            }]
        );
    }

    #[test]
    fn rows_reject_zero_bar_width() {
        let histogram = Histogram::from_samples(&[1.0], 1).unwrap();
        assert_eq!(histogram.rows(0, None), None);
    }

    #[test]
    fn rows_scale_bars_to_fixed_width_and_preserve_counts() {
        let histogram = Histogram::from_samples(&[0.0, 0.5, 1.0, 1.5, 1.75, 2.0], 2).unwrap();
        let rows = histogram.rows(10, None).unwrap();

        assert_eq!(
            rows,
            vec![
                HistogramRow {
                    label: "0.0".to_owned(),
                    bar: "#####".to_owned(),
                    count: 2,
                    marks_median: false
                },
                HistogramRow {
                    label: "1.0".to_owned(),
                    bar: "##########".to_owned(),
                    count: 4,
                    marks_median: false
                }
            ]
        );
    }

    #[test]
    fn rows_mark_median_in_matching_bin_only() {
        let histogram = Histogram::from_samples(&[0.0, 1.0, 2.0, 3.0], 4).unwrap();
        let rows = histogram.rows(8, Some(2.0)).unwrap();

        assert_eq!(rows.iter().filter(|row| row.marks_median).count(), 1);
        assert_eq!(rows[2].label, "1.5");
        assert!(rows[2].marks_median);
    }

    #[test]
    fn rows_mark_upper_bound_median_in_final_bin() {
        let histogram = Histogram::from_samples(&[0.0, 1.0, 2.0, 3.0], 4).unwrap();
        let rows = histogram.rows(8, Some(3.0)).unwrap();

        assert_eq!(rows.iter().filter(|row| row.marks_median).count(), 1);
        assert!(rows[3].marks_median);
    }

    #[test]
    fn render_ascii_uses_fixed_width_labels_and_median_suffix() {
        let histogram = Histogram::from_samples(&[9.0, 10.0, 10.5, 11.0], 2).unwrap();
        let lines = histogram.render_ascii(6, Some(10.0)).unwrap();

        assert_eq!(
            lines,
            vec![" 9.0 | ##".to_owned(), "10.0 | ######  median".to_owned()]
        );
    }

    #[test]
    fn narrow_histogram_uses_distinct_adaptive_labels() {
        let histogram = Histogram::from_samples(&[13.801, 13.804, 13.807, 13.810], 3).unwrap();
        let rows = histogram.rows(6, None).unwrap();

        assert_eq!(rows[0].label, "13.801");
        assert_eq!(rows[1].label, "13.804");
        assert_eq!(rows[2].label, "13.807");
    }

    #[test]
    fn free_function_returns_only_bins() {
        assert_eq!(
            bins(&[0.0, 1.0], 2).unwrap(),
            vec![
                HistogramBin {
                    lower: 0.0,
                    upper: 0.5,
                    count: 1
                },
                HistogramBin {
                    lower: 0.5,
                    upper: 1.0,
                    count: 1
                }
            ]
        );
    }
}
