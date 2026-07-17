use std::fmt;
use std::io;
use std::time::Duration;

use crate::output::BenchCase;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaseId(String);

impl CaseId {
    pub(crate) fn new(value: String) -> Self {
        Self(value)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CaseId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

pub enum BenchEvent<'a> {
    TopologyPlanned {
        device_count: usize,
        case_count: usize,
    },
    CaseStart {
        id: &'a CaseId,
        index: usize,
        total: usize,
    },
    WarmupStart {
        id: &'a CaseId,
        duration: Duration,
    },
    SamplingStart {
        id: &'a CaseId,
        samples: u32,
        estimated: Option<Duration>,
    },
    SamplingProgress {
        id: &'a CaseId,
        completed: u32,
        total: u32,
    },
    AnalysisStart {
        id: &'a CaseId,
    },
    CaseComplete {
        id: &'a CaseId,
        case: &'a BenchCase,
    },
    CaseSkipped {
        id: &'a CaseId,
        case: &'a BenchCase,
    },
    RunComplete {
        case_count: usize,
        measured_count: usize,
        skipped_count: usize,
    },
}

pub trait Report {
    fn event(&mut self, event: BenchEvent<'_>) -> io::Result<()>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoopReport;

impl Report for NoopReport {
    fn event(&mut self, _event: BenchEvent<'_>) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExecutionEvent {
    Warmup {
        duration: Duration,
    },
    Sampling {
        samples: u32,
        estimated: Option<Duration>,
    },
    SamplingProgress {
        completed: u32,
        total: u32,
    },
    Analysis,
}

pub(crate) struct EventFanout<'a> {
    report: &'a mut dyn Report,
    error: Option<io::Error>,
}

impl<'a> EventFanout<'a> {
    pub(crate) fn new(report: &'a mut dyn Report) -> Self {
        Self {
            report,
            error: None,
        }
    }

    pub(crate) fn emit(&mut self, event: BenchEvent<'_>) {
        if self.error.is_some() {
            return;
        }

        if let Err(error) = self.report.event(event) {
            self.error = Some(error);
        }
    }

    pub(crate) fn take_error(&mut self) -> io::Result<()> {
        if let Some(error) = self.error.take() {
            Err(error)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fanout_preserves_event_order_and_stops_after_error() {
        let mut seen = Vec::new();
        {
            let mut report = |event: BenchEvent<'_>| {
                match event {
                    BenchEvent::TopologyPlanned { .. } => seen.push("topology"),
                    BenchEvent::CaseStart { .. } => seen.push("case-start"),
                    BenchEvent::WarmupStart { .. } => {
                        seen.push("warmup");
                        return Err(io::Error::other("closed"));
                    }
                    BenchEvent::SamplingStart { .. } => seen.push("sampling"),
                    BenchEvent::SamplingProgress { .. } => seen.push("progress"),
                    BenchEvent::AnalysisStart { .. } => seen.push("analysis"),
                    BenchEvent::CaseComplete { .. }
                    | BenchEvent::CaseSkipped { .. }
                    | BenchEvent::RunComplete { .. } => seen.push("done"),
                }
                Ok(())
            };
            let id = CaseId::new("h2d/dev0/1B/engine-0/wall-clock".to_owned());
            let mut fanout = EventFanout::new(&mut report);

            fanout.emit(BenchEvent::TopologyPlanned {
                device_count: 1,
                case_count: 1,
            });
            fanout.emit(BenchEvent::CaseStart {
                id: &id,
                index: 1,
                total: 1,
            });
            fanout.emit(BenchEvent::WarmupStart {
                id: &id,
                duration: Duration::from_millis(1),
            });
            fanout.emit(BenchEvent::AnalysisStart { id: &id });
            assert!(fanout.take_error().is_err());
        }
        assert_eq!(seen, ["topology", "case-start", "warmup"]);
    }
}
