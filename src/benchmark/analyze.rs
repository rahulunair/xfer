use std::time::Duration;

use crate::output::CaseOutcome;
use crate::stats::{self, ConfidenceInterval, Summary};

use super::error::{BenchmarkError, CaseExecutionError};
use super::sampling::duration_to_gb_s;

pub(crate) fn analyze_durations(
    bytes: u64,
    durations: &[Duration],
    label: &str,
) -> std::result::Result<CaseOutcome, CaseExecutionError> {
    let samples_seconds = durations
        .iter()
        .map(Duration::as_secs_f64)
        .collect::<Vec<_>>();
    if samples_seconds.contains(&0.0) {
        return Err(CaseExecutionError::Fatal(BenchmarkError::Statistics(
            format!("{label} produced a zero-duration timing sample"),
        )));
    }
    let time_summary = stats::summarize(&samples_seconds).ok_or_else(|| {
        CaseExecutionError::Fatal(BenchmarkError::Statistics(format!(
            "{label} produced invalid duration statistics"
        )))
    })?;

    let samples_gb_s = durations
        .iter()
        .map(|duration| {
            duration_to_gb_s(bytes, *duration).ok_or_else(|| {
                CaseExecutionError::Fatal(BenchmarkError::Statistics(format!(
                    "{label} produced a zero-duration timing sample"
                )))
            })
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let summary = throughput_summary(bytes, &samples_gb_s, time_summary).ok_or_else(|| {
        CaseExecutionError::Fatal(BenchmarkError::Statistics(format!(
            "{label} produced invalid throughput statistics"
        )))
    })?;

    Ok(CaseOutcome::Measured {
        time_summary: Box::new(time_summary),
        summary,
        samples_gb_s,
    })
}

fn throughput_summary(bytes: u64, samples_gb_s: &[f64], time: Summary) -> Option<Summary> {
    let rate = |seconds: f64| bytes as f64 / seconds / 1_000_000_000.0;
    let median = rate(time.median);

    Some(Summary {
        count: samples_gb_s.len(),
        median,
        median_confidence: ConfidenceInterval {
            confidence_level: time.median_confidence.confidence_level,
            lower_bound: rate(time.median_confidence.upper_bound),
            upper_bound: rate(time.median_confidence.lower_bound),
            resamples: time.median_confidence.resamples,
        },
        mad: stats::median(
            &samples_gb_s
                .iter()
                .map(|sample| (sample - median).abs())
                .collect::<Vec<_>>(),
        )?,
        p5: stats::percentile(samples_gb_s, 5.0)?,
        p95: stats::percentile(samples_gb_s, 95.0)?,
        quartiles: stats::quartiles(samples_gb_s)?,
        outliers: stats::tukey_outliers(samples_gb_s)?,
        shape: stats::distribution_shape(samples_gb_s)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_is_computed_in_duration_space_before_rate_conversion() {
        let durations = [
            Duration::from_millis(1),
            Duration::from_millis(2),
            Duration::from_millis(4),
            Duration::from_millis(8),
            Duration::from_millis(16),
            Duration::from_millis(32),
            Duration::from_millis(64),
            Duration::from_millis(128),
            Duration::from_millis(256),
            Duration::from_millis(512),
        ];

        let CaseOutcome::Measured {
            time_summary,
            summary,
            ..
        } = analyze_durations(1_000_000_000, &durations, "test").unwrap()
        else {
            panic!("expected measured result");
        };

        assert_eq!(time_summary.median, 0.024);
        assert_eq!(summary.median, 1.0 / 0.024);
        assert_eq!(
            summary.median_confidence.lower_bound,
            1.0 / time_summary.median_confidence.upper_bound
        );
        assert_eq!(
            summary.median_confidence.upper_bound,
            1.0 / time_summary.median_confidence.lower_bound
        );
    }
}
