use std::time::Duration;

use crate::output::CaseOutcome;
use crate::stats;

use super::error::{BenchmarkError, CaseExecutionError};
use super::sampling::duration_to_gb_s;

pub(crate) fn analyze_durations(
    bytes: u64,
    durations: &[Duration],
    label: &str,
) -> std::result::Result<CaseOutcome, CaseExecutionError> {
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

    let summary = stats::summarize(&samples_gb_s).ok_or_else(|| {
        CaseExecutionError::Fatal(BenchmarkError::Statistics(format!(
            "{label} produced invalid sample statistics"
        )))
    })?;

    Ok(CaseOutcome::Measured {
        summary,
        samples_gb_s,
    })
}
