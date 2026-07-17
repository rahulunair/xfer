use std::io::{self, Write};
use std::time::Duration;

use crate::benchmark::{BenchEvent, Report};
use crate::cli::OutputFormat;
use indicatif::{ProgressBar, ProgressStyle};

use super::csv::{BENCH_CSV_HEADER, render_case_csv};
use super::model::{SystemInfo, TextOptions};
use super::text::{
    format_duration, render_case_text, render_summary_text, render_system_text,
    status_label_from_case_id,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatusMode {
    Interactive,
    Line,
    Disabled,
}

pub struct InteractiveReporter<R> {
    inner: R,
    progress: ProgressBar,
    spinner_style: ProgressStyle,
    sampling_style: ProgressStyle,
    complete_style: ProgressStyle,
}

impl<R> InteractiveReporter<R> {
    pub fn new(inner: R, color: super::model::ColorMode) -> Self {
        let (spinner_template, sampling_template, complete_template) = match color {
            super::model::ColorMode::Ansi => (
                "{spinner:.cyan} {msg}",
                "{spinner:.cyan} {msg} [{bar:20.cyan/bright_black}] {pos}/{len}",
                "{spinner:.green} {msg}",
            ),
            super::model::ColorMode::Never => (
                "{spinner} {msg}",
                "{spinner} {msg} [{bar:20}] {pos}/{len}",
                "{spinner} {msg}",
            ),
        };
        let spinner_style = ProgressStyle::with_template(spinner_template)
            .expect("static spinner template is valid")
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]);
        let sampling_style = ProgressStyle::with_template(sampling_template)
            .expect("static sampling template is valid")
            .progress_chars("█▉▊▋▌▍▎▏  ");
        let complete_style = ProgressStyle::with_template(complete_template)
            .expect("static completion template is valid")
            .tick_strings(&["✓"]);
        let progress = ProgressBar::new(0);
        progress.set_style(spinner_style.clone());
        progress.enable_steady_tick(Duration::from_millis(80));

        Self {
            inner,
            progress,
            spinner_style,
            sampling_style,
            complete_style,
        }
    }

    fn spinner(&self, message: String) {
        self.progress.reset();
        self.progress.set_style(self.spinner_style.clone());
        self.progress.set_message(message);
        self.progress.enable_steady_tick(Duration::from_millis(80));
    }
}

impl<R> Drop for InteractiveReporter<R> {
    fn drop(&mut self) {
        self.progress.finish_and_clear();
    }
}

impl<R> Report for InteractiveReporter<R>
where
    R: Report,
{
    fn event(&mut self, event: BenchEvent<'_>) -> io::Result<()> {
        let topology = match &event {
            BenchEvent::TopologyPlanned { system, case_count } => {
                Some((system.devices.len(), *case_count))
            }
            _ => None,
        };
        if let Some((device_count, case_count)) = topology {
            self.progress.finish_and_clear();
            self.inner.event(event)?;
            self.spinner(topology_message(device_count, case_count));
            return Ok(());
        }

        match &event {
            BenchEvent::TopologyPlanned { .. } => unreachable!("handled above"),
            BenchEvent::CaseStart { id, index, total } => self.spinner(format!(
                "Benchmarking {} ({index}/{total})",
                status_label_from_case_id(id.as_str())
            )),
            BenchEvent::WarmupStart { id, duration } => self.spinner(format!(
                "Benchmarking {}: Warming up for {}",
                status_label_from_case_id(id.as_str()),
                format_duration(*duration)
            )),
            BenchEvent::SamplingStart { id, samples, .. } => {
                self.progress.disable_steady_tick();
                self.progress.reset();
                self.progress.set_length(u64::from(*samples));
                self.progress.set_position(0);
                self.progress.set_style(self.sampling_style.clone());
                self.progress.set_message(format!(
                    "Benchmarking {}: Collecting",
                    status_label_from_case_id(id.as_str())
                ));
            }
            BenchEvent::SamplingProgress {
                completed, total, ..
            } => {
                self.progress.set_length(u64::from(*total));
                self.progress.set_position(u64::from(*completed));
            }
            BenchEvent::AnalysisStart { id } => self.spinner(format!(
                "Benchmarking {}: Analyzing",
                status_label_from_case_id(id.as_str())
            )),
            BenchEvent::CaseComplete { .. } | BenchEvent::CaseSkipped { .. } => {
                self.progress.finish_and_clear();
            }
            BenchEvent::RunComplete {
                case_count,
                measured_count,
                skipped_count,
            } => {
                self.progress.reset();
                self.progress.set_style(self.complete_style.clone());
                self.progress.finish_with_message(completion_message(
                    *case_count,
                    *measured_count,
                    *skipped_count,
                ));
            }
        }

        self.inner.event(event)
    }
}

pub struct LiveReporter<W, E> {
    stdout: W,
    stderr: E,
    format: OutputFormat,
    text_options: TextOptions,
    status_mode: StatusMode,
    progress_enabled: bool,
    output_state: OutputState,
    summary_cases: Vec<crate::output::BenchCase>,
}

#[derive(Default)]
struct OutputState {
    wrote_csv_header: bool,
    wrote_system: bool,
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
            output_state: OutputState::default(),
            summary_cases: Vec::new(),
        }
    }

    pub fn into_inner(self) -> (W, E) {
        (self.stdout, self.stderr)
    }

    fn emit_csv_header(&mut self) -> io::Result<()> {
        if self.output_state.wrote_csv_header {
            return Ok(());
        }

        self.stdout.write_all(BENCH_CSV_HEADER.as_bytes())?;
        self.stdout.write_all(b"\n")?;
        self.stdout.flush()?;
        self.output_state.wrote_csv_header = true;
        Ok(())
    }

    fn emit_case(&mut self, case: &crate::output::BenchCase) -> io::Result<()> {
        match self.format {
            OutputFormat::Text => {
                if self.text_options.summary_only {
                    return Ok(());
                }
                self.clear_interactive_status()?;
                if self.output_state.wrote_text_case {
                    self.stdout.write_all(b"\n")?;
                }
                self.stdout
                    .write_all(render_case_text(case, &self.text_options).as_bytes())?;
                self.stdout.flush()?;
                self.output_state.wrote_text_case = true;
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

    fn emit_system(&mut self, system: &SystemInfo) -> io::Result<()> {
        if self.format != OutputFormat::Text || self.output_state.wrote_system {
            return Ok(());
        }

        self.clear_interactive_status()?;
        self.stdout
            .write_all(render_system_text(system, self.text_options.color).as_bytes())?;
        self.stdout.flush()?;
        self.output_state.wrote_system = true;
        Ok(())
    }

    fn emit_summary(&mut self) -> io::Result<()> {
        if self.format != OutputFormat::Text
            || (!self.text_options.summary_only && self.summary_cases.len() <= 1)
        {
            return Ok(());
        }

        self.clear_interactive_status()?;
        if self.output_state.wrote_text_case {
            self.stdout.write_all(b"\n")?;
        }
        let summary = render_summary_text(&self.summary_cases, &self.text_options);
        self.stdout.write_all(summary.as_bytes())?;
        self.stdout.flush()?;
        self.output_state.wrote_text_case = true;
        Ok(())
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
            BenchEvent::TopologyPlanned { system, case_count } => match self.format {
                OutputFormat::Text => {
                    self.emit_system(system)?;
                    self.status(&topology_message(system.devices.len(), case_count))
                }
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
                    format!(" in estimated {}", format_duration(duration))
                });
                self.status(&format!(
                    "Benchmarking {}: Collecting {samples} samples{estimate}",
                    status_label_from_case_id(id.as_str())
                ))
            }
            BenchEvent::SamplingProgress {
                id,
                completed,
                total,
            } => {
                if self.status_mode != StatusMode::Interactive {
                    return Ok(());
                }
                self.status(&format!(
                    "Benchmarking {}: Collecting {} {completed}/{total} samples",
                    status_label_from_case_id(id.as_str()),
                    progress_bar(completed, total)
                ))
            }
            BenchEvent::AnalysisStart { id } => self.status(&format!(
                "Benchmarking {}: Analyzing",
                status_label_from_case_id(id.as_str())
            )),
            BenchEvent::CaseComplete { case, .. } | BenchEvent::CaseSkipped { case, .. } => {
                if self.format == OutputFormat::Text {
                    self.summary_cases.push(case.clone());
                }
                self.emit_case(case)
            }
            BenchEvent::RunComplete {
                case_count,
                measured_count,
                skipped_count,
            } => {
                self.emit_summary()?;
                self.finish_status(&completion_message(
                    case_count,
                    measured_count,
                    skipped_count,
                ))
            }
        }
    }
}

fn topology_message(device_count: usize, case_count: usize) -> String {
    format!(
        "Topology planned: {device_count} {}, {case_count} {}",
        plural(device_count, "device", "devices"),
        plural(case_count, "case", "cases")
    )
}

fn completion_message(case_count: usize, measured_count: usize, skipped_count: usize) -> String {
    format!(
        "Complete: {case_count} {}, {measured_count} measured, {skipped_count} skipped",
        plural(case_count, "case", "cases")
    )
}

fn plural<'word>(count: usize, singular: &'word str, plural: &'word str) -> &'word str {
    if count == 1 { singular } else { plural }
}

fn progress_bar(completed: u32, total: u32) -> String {
    const WIDTH: usize = 20;
    let filled = if total == 0 {
        0
    } else {
        (completed.min(total) as usize * WIDTH) / total as usize
    };
    format!("[{}{}]", "=".repeat(filled), " ".repeat(WIDTH - filled))
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
            mode: crate::cli::BenchMode::Single,
            selected_group: Some(QueueGroupInfo {
                ordinal: 2,
                flags: QueueFlags {
                    copy: true,
                    compute: false,
                },
                queue_count: 1,
            }),
            streams: vec![crate::output::QueueStreamInfo {
                group_ordinal: 2,
                queue_index: 0,
                flags: QueueFlags {
                    copy: true,
                    compute: false,
                },
            }],
            second_phase_streams: Vec::new(),
            verification_stream: None,
            transfer_class: TransferClass::H2D,
            operation: Operation::HostToDevice,
            source: Endpoint::Host,
            destination: Endpoint::Device(0),
            byte_count: 1024,
            allocation: AllocationKind::PinnedHost,
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
        let id =
            CaseId::new("h2d/host-to-dev0/1KiB/group-2/wall-clock/single-streams-1".to_owned());
        let case = measured_case();
        let system = crate::output::test_system(1);
        let mut reporter = LiveReporter::new(
            Vec::new(),
            Vec::new(),
            OutputFormat::Text,
            TextOptions {
                include_histogram: false,
                summary_only: false,
                color: ColorMode::Never,
            },
            StatusMode::Interactive,
        );

        reporter
            .event(BenchEvent::TopologyPlanned {
                system: &system,
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
        assert!(stdout.starts_with("System under test\n"));
        assert_eq!(stdout.matches("System under test").count(), 1);
        assert!(stdout.contains("H2D pinned host -> dev0"));
        assert!(stdout.contains("engine 2 / queue 0 (copy)"));
        assert!(stderr.contains("\r\u{1b}[2KTopology planned"));
        assert!(stderr.contains("\r\u{1b}[2KBenchmarking H2D host -> dev0 / engine 2"));
        assert!(stderr.ends_with("\r\u{1b}[2K"));
        assert!(!stderr.contains("ordinal"));
    }

    #[test]
    fn non_tty_status_uses_newline_lifecycle() {
        let id =
            CaseId::new("h2d/host-to-dev0/1KiB/group-2/wall-clock/single-streams-1".to_owned());
        let system = crate::output::test_system(1);
        let mut reporter = LiveReporter::new(
            Vec::new(),
            Vec::new(),
            OutputFormat::Text,
            TextOptions::default(),
            StatusMode::Line,
        );

        reporter
            .event(BenchEvent::TopologyPlanned {
                system: &system,
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
            "Topology planned: 1 device, 1 case\nBenchmarking H2D host -> dev0 / engine 2 (1/1)\nBenchmarking H2D host -> dev0 / engine 2: Warming up for 10 ms\nBenchmarking H2D host -> dev0 / engine 2: Collecting 3 samples in estimated 30 ms\nBenchmarking H2D host -> dev0 / engine 2: Analyzing\nComplete: 1 case, 1 measured, 0 skipped\n"
        );
    }

    #[test]
    fn tty_sampling_progress_overwrites_with_verified_sample_count() {
        let id =
            CaseId::new("h2d/host-to-dev0/1KiB/group-2/wall-clock/single-streams-1".to_owned());
        let mut reporter = LiveReporter::new(
            Vec::new(),
            Vec::new(),
            OutputFormat::Text,
            TextOptions::default(),
            StatusMode::Interactive,
        );

        reporter
            .event(BenchEvent::SamplingProgress {
                id: &id,
                completed: 25,
                total: 50,
            })
            .expect("progress");

        let (_, stderr) = reporter.into_inner();
        let stderr = String::from_utf8(stderr).expect("stderr utf8");
        assert_eq!(
            stderr,
            "\r\u{1b}[2KBenchmarking H2D host -> dev0 / engine 2: Collecting [==========          ] 25/50 samples"
        );
    }

    #[test]
    fn csv_reporter_writes_one_header_rows_and_no_progress() {
        let id =
            CaseId::new("h2d/host-to-dev0/1KiB/group-2/wall-clock/single-streams-1".to_owned());
        let case = measured_case();
        let system = crate::output::test_system(1);
        let mut reporter = LiveReporter::new(
            Vec::new(),
            Vec::new(),
            OutputFormat::Csv,
            TextOptions::default(),
            StatusMode::Interactive,
        );

        reporter
            .event(BenchEvent::TopologyPlanned {
                system: &system,
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
    fn summary_only_emits_one_copy_safe_table_at_run_completion() {
        let id =
            CaseId::new("h2d/host-to-dev0/1KiB/group-2/wall-clock/single-streams-1".to_owned());
        let case = measured_case();
        let system = crate::output::test_system(1);
        let mut reporter = LiveReporter::new(
            Vec::new(),
            Vec::new(),
            OutputFormat::Text,
            TextOptions {
                include_histogram: true,
                summary_only: true,
                color: ColorMode::Never,
            },
            StatusMode::Disabled,
        );

        reporter
            .event(BenchEvent::TopologyPlanned {
                system: &system,
                case_count: 1,
            })
            .expect("topology");
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
            .expect("run complete");

        let (stdout, stderr) = reporter.into_inner();
        let stdout = String::from_utf8(stdout).expect("stdout utf8");
        assert!(stdout.starts_with("System under test\n"));
        assert_eq!(stdout.matches("System under test").count(), 1);
        assert!(stdout.contains("Run summary"));
        assert!(stdout.contains("H2D host -> dev0"));
        assert!(!stdout.contains("config      mode="));
        assert!(!stdout.contains("distribution"));
        assert!(!stdout.contains("\u{1b}["));
        assert!(stderr.is_empty());
    }

    #[test]
    fn stdout_broken_pipe_is_reported_to_caller() {
        let id =
            CaseId::new("h2d/host-to-dev0/1KiB/group-2/wall-clock/single-streams-1".to_owned());
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
        let id =
            CaseId::new("h2d/host-to-dev0/1KiB/group-2/wall-clock/single-streams-1".to_owned());
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
