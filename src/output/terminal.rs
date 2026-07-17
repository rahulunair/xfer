use std::io::{self, Write};

use crate::benchmark::{BenchEvent, Report};
use crate::cli::OutputFormat;

use super::csv::{BENCH_CSV_HEADER, render_case_csv};
use super::model::TextOptions;
use super::text::{format_duration, render_case_text, status_label_from_case_id};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatusMode {
    Interactive,
    Line,
    Disabled,
}

pub struct LiveReporter<W, E> {
    stdout: W,
    stderr: E,
    format: OutputFormat,
    text_options: TextOptions,
    status_mode: StatusMode,
    progress_enabled: bool,
    wrote_csv_header: bool,
    wrote_text_case: bool,
}

impl<W, E> LiveReporter<W, E>
where
    W: Write,
    E: Write,
{
    pub fn new(
        stdout: W,
        stderr: E,
        format: OutputFormat,
        text_options: TextOptions,
        status_mode: StatusMode,
    ) -> Self {
        Self {
            stdout,
            stderr,
            format,
            text_options,
            status_mode,
            progress_enabled: status_mode != StatusMode::Disabled,
            wrote_csv_header: false,
            wrote_text_case: false,
        }
    }

    pub fn into_inner(self) -> (W, E) {
        (self.stdout, self.stderr)
    }

    fn emit_csv_header(&mut self) -> io::Result<()> {
        if self.wrote_csv_header {
            return Ok(());
        }

        self.stdout.write_all(BENCH_CSV_HEADER.as_bytes())?;
        self.stdout.write_all(b"\n")?;
        self.stdout.flush()?;
        self.wrote_csv_header = true;
        Ok(())
    }

    fn emit_case(&mut self, case: &crate::output::BenchCase) -> io::Result<()> {
        match self.format {
            OutputFormat::Text => {
                self.clear_interactive_status()?;
                if self.wrote_text_case {
                    self.stdout.write_all(b"\n")?;
                }
                self.stdout
                    .write_all(render_case_text(case, &self.text_options).as_bytes())?;
                self.stdout.flush()?;
                self.wrote_text_case = true;
                Ok(())
            }
            OutputFormat::Csv => {
                self.emit_csv_header()?;
                self.stdout.write_all(render_case_csv(case).as_bytes())?;
                self.stdout.write_all(b"\n")?;
                self.stdout.flush()
            }
        }
    }

    fn status(&mut self, message: &str) -> io::Result<()> {
        if self.format == OutputFormat::Csv || !self.progress_enabled {
            return Ok(());
        }

        let result = match self.status_mode {
            StatusMode::Interactive => {
                write!(self.stderr, "\r\u{1b}[2K{message}").and_then(|()| self.stderr.flush())
            }
            StatusMode::Line => {
                writeln!(self.stderr, "{message}").and_then(|()| self.stderr.flush())
            }
            StatusMode::Disabled => Ok(()),
        };

        if let Err(error) = result {
            if error.kind() == io::ErrorKind::BrokenPipe {
                self.progress_enabled = false;
                Ok(())
            } else {
                Err(error)
            }
        } else {
            Ok(())
        }
    }

    fn clear_interactive_status(&mut self) -> io::Result<()> {
        if self.format != OutputFormat::Text
            || !self.progress_enabled
            || self.status_mode != StatusMode::Interactive
        {
            return Ok(());
        }

        let result = write!(self.stderr, "\r\u{1b}[2K").and_then(|()| self.stderr.flush());
        match result {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {
                self.progress_enabled = false;
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    fn finish_status(&mut self, message: &str) -> io::Result<()> {
        if self.format == OutputFormat::Csv || !self.progress_enabled {
            return Ok(());
        }

        if self.status_mode == StatusMode::Interactive {
            self.status(message)?;
            self.stderr.write_all(b"\n")?;
            self.stderr.flush()
        } else {
            self.status(message)
        }
    }
}

impl<W, E> Report for LiveReporter<W, E>
where
    W: Write,
    E: Write,
{
    fn event(&mut self, event: BenchEvent<'_>) -> io::Result<()> {
        match event {
            BenchEvent::TopologyPlanned {
                device_count,
                case_count,
            } => match self.format {
                OutputFormat::Text => self.status(&format!(
                    "Topology planned: {device_count} devices, {case_count} cases"
                )),
                OutputFormat::Csv => self.emit_csv_header(),
            },
            BenchEvent::CaseStart { id, index, total } => self.status(&format!(
                "Benchmarking {} ({index}/{total})",
                status_label_from_case_id(id.as_str())
            )),
            BenchEvent::WarmupStart { id, duration } => self.status(&format!(
                "Benchmarking {}: Warming up for {}",
                status_label_from_case_id(id.as_str()),
                format_duration(duration)
            )),
            BenchEvent::SamplingStart {
                id,
                samples,
                estimated,
            } => {
                let estimate = estimated.map_or_else(String::new, |duration| {
                    format!(" in approximately {}", format_duration(duration))
                });
                self.status(&format!(
                    "Benchmarking {}: Collecting {samples} samples{estimate}",
                    status_label_from_case_id(id.as_str())
                ))
            }
            BenchEvent::AnalysisStart { id } => self.status(&format!(
                "Benchmarking {}: Analyzing",
                status_label_from_case_id(id.as_str())
            )),
            BenchEvent::CaseComplete { case, .. } | BenchEvent::CaseSkipped { case, .. } => {
                self.emit_case(case)
            }
            BenchEvent::RunComplete {
                case_count,
                measured_count,
                skipped_count,
            } => self.finish_status(&format!(
                "Complete: {case_count} cases, {measured_count} measured, {skipped_count} skipped"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::time::Duration;

    use crate::benchmark::{BenchEvent, CaseId, Report};
    use crate::cli::{OutputFormat, TimingMode, TransferClass};
    use crate::output::{
        AllocationKind, BenchCase, CaseOutcome, ColorMode, Endpoint, LinkInfo, Operation,
        QueueFlags, QueueGroupInfo, TextOptions,
    };
    use crate::stats;

    use super::*;

    fn measured_case() -> BenchCase {
        let samples = vec![10.0, 20.0, 40.0];
        let summary = stats::summarize(&samples).expect("summary");

        BenchCase {
            transfer_class: TransferClass::H2D,
            operation: Operation::HostToDevice,
            source: Endpoint::Host,
            destination: Endpoint::Device(0),
            byte_count: 1024,
            allocation: AllocationKind::PinnedHost,
            queue: QueueGroupInfo {
                ordinal: 2,
                flags: QueueFlags {
                    copy: true,
                    compute: false,
                },
            },
            timing: TimingMode::WallClock,
            warmup: Duration::from_millis(10),
            requested_samples: 3,
            pcie_link: LinkInfo::Unknown {
                reason: "test".to_owned(),
            },
            outcome: CaseOutcome::Measured {
                time_summary: Box::new(summary),
                summary,
                samples_gb_s: samples,
            },
        }
    }

    #[test]
    fn tty_status_overwrites_and_streams_text_cases() {
        let id = CaseId::new("h2d/host-to-dev0/1KiB/engine-2/wall-clock".to_owned());
        let case = measured_case();
        let mut reporter = LiveReporter::new(
            Vec::new(),
            Vec::new(),
            OutputFormat::Text,
            TextOptions {
                include_histogram: false,
                color: ColorMode::Never,
            },
            StatusMode::Interactive,
        );

        reporter
            .event(BenchEvent::TopologyPlanned {
                device_count: 1,
                case_count: 1,
            })
            .expect("topology");
        reporter
            .event(BenchEvent::CaseStart {
                id: &id,
                index: 1,
                total: 1,
            })
            .expect("case start");
        reporter
            .event(BenchEvent::CaseComplete {
                id: &id,
                case: &case,
            })
            .expect("case complete");

        let (stdout, stderr) = reporter.into_inner();
        let stdout = String::from_utf8(stdout).expect("stdout utf8");
        let stderr = String::from_utf8(stderr).expect("stderr utf8");
        assert!(stdout.starts_with("H2D pinned host -> dev0"));
        assert!(stdout.contains("copy engine 2"));
        assert!(stderr.contains("\r\u{1b}[2KTopology planned"));
        assert!(stderr.contains("\r\u{1b}[2KBenchmarking H2D host -> dev0 / engine 2"));
        assert!(stderr.ends_with("\r\u{1b}[2K"));
        assert!(!stderr.contains("ordinal"));
    }

    #[test]
    fn non_tty_status_uses_newline_lifecycle() {
        let id = CaseId::new("h2d/host-to-dev0/1KiB/engine-2/wall-clock".to_owned());
        let mut reporter = LiveReporter::new(
            Vec::new(),
            Vec::new(),
            OutputFormat::Text,
            TextOptions::default(),
            StatusMode::Line,
        );

        reporter
            .event(BenchEvent::TopologyPlanned {
                device_count: 1,
                case_count: 1,
            })
            .expect("topology");
        reporter
            .event(BenchEvent::CaseStart {
                id: &id,
                index: 1,
                total: 1,
            })
            .expect("case start");
        reporter
            .event(BenchEvent::WarmupStart {
                id: &id,
                duration: Duration::from_millis(10),
            })
            .expect("warmup");
        reporter
            .event(BenchEvent::SamplingStart {
                id: &id,
                samples: 3,
                estimated: Some(Duration::from_millis(30)),
            })
            .expect("sampling");
        reporter
            .event(BenchEvent::AnalysisStart { id: &id })
            .expect("analysis");
        reporter
            .event(BenchEvent::RunComplete {
                case_count: 1,
                measured_count: 1,
                skipped_count: 0,
            })
            .expect("complete");

        let (_, stderr) = reporter.into_inner();
        let stderr = String::from_utf8(stderr).expect("stderr utf8");
        assert_eq!(
            stderr,
            "Topology planned: 1 devices, 1 cases\nBenchmarking H2D host -> dev0 / engine 2 (1/1)\nBenchmarking H2D host -> dev0 / engine 2: Warming up for 10 ms\nBenchmarking H2D host -> dev0 / engine 2: Collecting 3 samples in approximately 30 ms\nBenchmarking H2D host -> dev0 / engine 2: Analyzing\nComplete: 1 cases, 1 measured, 0 skipped\n"
        );
    }

    #[test]
    fn csv_reporter_writes_one_header_rows_and_no_progress() {
        let id = CaseId::new("h2d/host-to-dev0/1KiB/engine-2/wall-clock".to_owned());
        let case = measured_case();
        let mut reporter = LiveReporter::new(
            Vec::new(),
            Vec::new(),
            OutputFormat::Csv,
            TextOptions::default(),
            StatusMode::Interactive,
        );

        reporter
            .event(BenchEvent::TopologyPlanned {
                device_count: 1,
                case_count: 1,
            })
            .expect("topology");
        reporter
            .event(BenchEvent::CaseStart {
                id: &id,
                index: 1,
                total: 1,
            })
            .expect("case start");
        reporter
            .event(BenchEvent::CaseComplete {
                id: &id,
                case: &case,
            })
            .expect("case complete");
        reporter
            .event(BenchEvent::RunComplete {
                case_count: 1,
                measured_count: 1,
                skipped_count: 0,
            })
            .expect("complete");

        let (stdout, stderr) = reporter.into_inner();
        let stdout = String::from_utf8(stdout).expect("stdout utf8");
        let stderr = String::from_utf8(stderr).expect("stderr utf8");
        assert_eq!(stdout.lines().next(), Some(BENCH_CSV_HEADER));
        assert_eq!(stdout.lines().count(), 2);
        assert!(stderr.is_empty());
        assert!(!stdout.contains("\u{1b}["));
    }

    #[test]
    fn stdout_broken_pipe_is_reported_to_caller() {
        let id = CaseId::new("h2d/host-to-dev0/1KiB/engine-2/wall-clock".to_owned());
        let case = measured_case();
        let mut reporter = LiveReporter::new(
            BrokenPipeWriter,
            Vec::new(),
            OutputFormat::Text,
            TextOptions::default(),
            StatusMode::Disabled,
        );

        let error = reporter
            .event(BenchEvent::CaseComplete {
                id: &id,
                case: &case,
            })
            .expect_err("stdout should fail");
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn progress_broken_pipe_disables_status_without_failing_benchmark() {
        let id = CaseId::new("h2d/host-to-dev0/1KiB/engine-2/wall-clock".to_owned());
        let mut reporter = LiveReporter::new(
            Vec::new(),
            BrokenPipeWriter,
            OutputFormat::Text,
            TextOptions::default(),
            StatusMode::Line,
        );

        reporter
            .event(BenchEvent::CaseStart {
                id: &id,
                index: 1,
                total: 1,
            })
            .expect("stderr broken pipe is ignored for progress");
        reporter
            .event(BenchEvent::WarmupStart {
                id: &id,
                duration: Duration::from_millis(10),
            })
            .expect("progress stays disabled");
    }

    struct BrokenPipeWriter;

    impl Write for BrokenPipeWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
}
