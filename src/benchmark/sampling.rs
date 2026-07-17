use std::time::{Duration, Instant};

use super::error::CaseExecutionError;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct WarmupStats {
    elapsed: Duration,
    iterations: u64,
}

pub(crate) fn warmup<F>(
    duration: Duration,
    mut run_once: F,
) -> std::result::Result<WarmupStats, CaseExecutionError>
where
    F: FnMut() -> std::result::Result<(), CaseExecutionError>,
{
    if duration.is_zero() {
        return Ok(WarmupStats::default());
    }

    let started = Instant::now();
    let mut iterations = 0_u64;
    while started.elapsed() < duration {
        run_once()?;
        iterations = iterations.saturating_add(1);
    }

    Ok(WarmupStats {
        elapsed: started.elapsed(),
        iterations,
    })
}

pub(crate) fn estimate_collection(warmup: WarmupStats, samples: u32) -> Option<Duration> {
    if warmup.iterations == 0 {
        return None;
    }

    Some(Duration::from_secs_f64(
        warmup.elapsed.as_secs_f64() / warmup.iterations as f64 * f64::from(samples),
    ))
}

pub(crate) fn duration_to_gb_s(bytes: u64, duration: Duration) -> Option<f64> {
    let seconds = duration.as_secs_f64();
    if seconds > 0.0 {
        Some(bytes as f64 / seconds / 1_000_000_000.0)
    } else {
        None
    }
}

pub(crate) fn sample_capacity(samples: u32) -> std::result::Result<usize, CaseExecutionError> {
    usize::try_from(samples).map_err(|_| {
        CaseExecutionError::Fatal(super::error::BenchmarkError::Statistics(
            "sample count does not fit usize".to_owned(),
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_to_gb_s_uses_decimal_units() {
        assert_eq!(
            duration_to_gb_s(1_000_000_000, Duration::from_secs(2)),
            Some(0.5)
        );
    }

    #[test]
    fn duration_to_gb_s_rejects_zero_duration() {
        assert_eq!(duration_to_gb_s(1, Duration::ZERO), None);
    }

    #[test]
    fn zero_warmup_has_no_collection_estimate() {
        assert_eq!(estimate_collection(WarmupStats::default(), 10), None);
    }

    #[test]
    fn nonzero_warmup_estimates_collection_time() {
        let stats = WarmupStats {
            elapsed: Duration::from_millis(25),
            iterations: 5,
        };

        assert_eq!(
            estimate_collection(stats, 4),
            Some(Duration::from_millis(20))
        );
    }
}
