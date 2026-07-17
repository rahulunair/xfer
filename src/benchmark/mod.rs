#![allow(
    clippy::cast_precision_loss,
    clippy::missing_errors_doc,
    clippy::module_name_repetitions
)]

//! Benchmark orchestration for xfer.
//!
//! Formatting stays in `output`, statistics stay in `stats`, sysfs `PCIe`
//! discovery stays in `pcie`, and raw Level Zero command recording is confined
//! to `command`.

mod analyze;
mod command;
mod error;
mod event;
mod execute;
mod plan;
mod sampling;
mod topology;
mod verify;

use std::io;

use crate::cli::{BenchOptions, MIN_SAMPLES};
use crate::output::{BenchReport, CaseOutcome, DeviceInfo, ListReport};

use self::error::CaseExecutionError;
pub use self::error::{BenchmarkError, Result};
pub use self::event::{BenchEvent, CaseId, NoopReport, Report};
use self::event::{BenchEvent as Event, EventFanout, ExecutionEvent};
use self::plan::{joined_skip_reasons, plan_cases, validate_filters};
use self::topology::discover_topology;

pub fn list() -> Result<ListReport> {
    let topology = discover_topology()?;
    Ok(ListReport {
        devices: topology
            .devices
            .iter()
            .map(|device| DeviceInfo {
                index: device.index,
                name: device.properties.name.clone(),
                pci_address: device.pci_address.clone(),
                pcie_link: device.pcie_link.clone(),
                queue_groups: device
                    .queues
                    .iter()
                    .map(|queue| queue.info.clone())
                    .collect(),
            })
            .collect(),
        peer_access: topology.peer_access,
    })
}

pub fn bench(options: &BenchOptions) -> Result<BenchReport> {
    bench_with_reporter(options, NoopReport)
}

pub fn bench_with_reporter<R>(options: &BenchOptions, mut reporter: R) -> Result<BenchReport>
where
    R: Report,
{
    validate_options(options)?;
    let topology = discover_topology()?;
    if topology.devices.is_empty() {
        return Err(BenchmarkError::NoDevices);
    }

    validate_filters(&topology, options)?;
    let byte_count = usize::try_from(options.size_bytes)
        .map_err(|_| BenchmarkError::SizeTooLarge(options.size_bytes))?;
    let plans = plan_cases(&topology, options);
    let total = plans.len();
    let mut cases = Vec::with_capacity(total);
    let mut fanout = EventFanout::new(&mut reporter);

    fanout.emit(Event::TopologyPlanned {
        device_count: topology.devices.len(),
        case_count: total,
    });
    reporter_ok(&mut fanout)?;

    for (index, plan) in plans.into_iter().enumerate() {
        let id = plan.case_id(options);
        fanout.emit(Event::CaseStart {
            id: &id,
            index: index + 1,
            total,
        });
        reporter_ok(&mut fanout)?;

        let outcome = if let Some(reason) = joined_skip_reasons(&plan.skip_reasons) {
            CaseOutcome::Skipped { reason }
        } else {
            {
                let mut events = |event| emit_execution_event(&mut fanout, &id, event);
                match execute::execute_case(&topology, &plan, options, byte_count, &mut events) {
                    Ok(outcome) => outcome,
                    Err(CaseExecutionError::Skip(reason)) => CaseOutcome::Skipped { reason },
                    Err(CaseExecutionError::Fatal(error)) => {
                        if let Some(reason) = error::capability_skip_reason(&error) {
                            CaseOutcome::Skipped { reason }
                        } else {
                            return Err(error);
                        }
                    }
                }
            }
        };
        reporter_ok(&mut fanout)?;

        let case = plan.into_case(options, outcome);
        match &case.outcome {
            CaseOutcome::Measured { .. } => fanout.emit(Event::CaseComplete {
                id: &id,
                case: &case,
            }),
            CaseOutcome::Skipped { .. } => fanout.emit(Event::CaseSkipped {
                id: &id,
                case: &case,
            }),
        }
        reporter_ok(&mut fanout)?;
        cases.push(case);
    }

    fanout.emit(Event::RunComplete {
        case_count: cases.len(),
        measured_count: cases
            .iter()
            .filter(|case| matches!(case.outcome, CaseOutcome::Measured { .. }))
            .count(),
        skipped_count: cases
            .iter()
            .filter(|case| matches!(case.outcome, CaseOutcome::Skipped { .. }))
            .count(),
    });
    reporter_ok(&mut fanout)?;

    Ok(BenchReport { cases })
}

fn validate_options(options: &BenchOptions) -> Result<()> {
    if options.samples < MIN_SAMPLES {
        Err(BenchmarkError::InvalidFilter(format!(
            "sample count must be at least {MIN_SAMPLES}"
        )))
    } else {
        Ok(())
    }
}

fn emit_execution_event(fanout: &mut EventFanout<'_>, id: &CaseId, event: ExecutionEvent) {
    match event {
        ExecutionEvent::Warmup { duration } => {
            fanout.emit(Event::WarmupStart { id, duration });
        }
        ExecutionEvent::Sampling { samples, estimated } => {
            fanout.emit(Event::SamplingStart {
                id,
                samples,
                estimated,
            });
        }
        ExecutionEvent::Analysis => {
            fanout.emit(Event::AnalysisStart { id });
        }
    }
}

fn reporter_ok(fanout: &mut EventFanout<'_>) -> Result<()> {
    fanout.take_error().map_err(BenchmarkError::Reporter)
}

impl<F> Report for F
where
    F: FnMut(BenchEvent<'_>) -> io::Result<()>,
{
    fn event(&mut self, event: BenchEvent<'_>) -> io::Result<()> {
        self(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_api_enforces_minimum_sample_count() {
        let options = BenchOptions {
            samples: MIN_SAMPLES - 1,
            ..BenchOptions::default()
        };

        assert!(matches!(
            validate_options(&options),
            Err(BenchmarkError::InvalidFilter(message))
                if message == "sample count must be at least 10"
        ));
    }
}
