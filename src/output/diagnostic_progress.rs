use std::io::{self, Write};

use crate::benchmark::{BenchEvent, Report};
use crate::cli::OutputFormat;
use crate::diagnostics::{
    DiagnosticPhase, DiagnosticPhaseStatus, EvidenceFailure, P2pDiagnosticProgress, phase_position,
};

use super::model::ColorMode;
use super::progress::{IndicatifProgress, ascii_progress_bar, write_status};
use super::terminal::StatusMode;
use super::text::format_duration;

pub struct DiagnosticProgressReporter<W> {
    stderr: W,
    format: OutputFormat,
    status_mode: StatusMode,
    settings: String,
    current_phase: Option<DiagnosticPhase>,
    progress_enabled: bool,
}

impl<W> DiagnosticProgressReporter<W>
where
    W: Write,
{
    pub fn new(stderr: W, format: OutputFormat, status_mode: StatusMode, settings: String) -> Self {
        Self {
            stderr,
            format,
            status_mode,
            settings,
            current_phase: None,
            progress_enabled: status_mode != StatusMode::Disabled,
        }
    }

    pub fn into_inner(self) -> W {
        self.stderr
    }

    fn status(&mut self, message: &str) -> io::Result<()> {
        if self.format == OutputFormat::Csv || !self.progress_enabled {
            return Ok(());
        }
        write_status(
            &mut self.stderr,
            self.status_mode,
            &mut self.progress_enabled,
            message,
            false,
        )
    }

    fn finish_status(&mut self, message: &str) -> io::Result<()> {
        if self.format == OutputFormat::Csv || !self.progress_enabled {
            return Ok(());
        }

        write_status(
            &mut self.stderr,
            self.status_mode,
            &mut self.progress_enabled,
            message,
            true,
        )
    }

    fn current_phase_message(&self, action: &str) -> Option<String> {
        self.current_phase
            .map(|phase| phase_message(phase, action, &self.settings))
    }
}

impl<W> Report for DiagnosticProgressReporter<W>
where
    W: Write,
{
    fn event(&mut self, event: BenchEvent<'_>) -> io::Result<()> {
        match event {
            BenchEvent::WarmupStart { duration, .. } => {
                if let Some(message) = self
                    .current_phase_message(&format!("warming up for {}", format_duration(duration)))
                {
                    self.status(&message)
                } else {
                    Ok(())
                }
            }
            BenchEvent::SamplingStart { samples, .. } => {
                if let Some(message) =
                    self.current_phase_message(&format!("collecting {samples} verified samples"))
                {
                    self.status(&message)
                } else {
                    Ok(())
                }
            }
            BenchEvent::SamplingProgress {
                completed, total, ..
            } => {
                if self.status_mode != StatusMode::Interactive {
                    return Ok(());
                }
                if let Some(message) = self.current_phase_message(&format!(
                    "collecting verified samples {} {completed}/{total}",
                    ascii_progress_bar(completed, total)
                )) {
                    self.status(&message)
                } else {
                    Ok(())
                }
            }
            BenchEvent::AnalysisStart { .. } => {
                if let Some(message) = self.current_phase_message("analyzing") {
                    self.status(&message)
                } else {
                    Ok(())
                }
            }
            BenchEvent::TopologyPlanned { .. }
            | BenchEvent::CaseStart { .. }
            | BenchEvent::CaseComplete { .. }
            | BenchEvent::CaseSkipped { .. }
            | BenchEvent::RunComplete { .. } => Ok(()),
        }
    }
}

impl<W> P2pDiagnosticProgress for DiagnosticProgressReporter<W>
where
    W: Write,
{
    fn phase_started(&mut self, phase: DiagnosticPhase) -> io::Result<()> {
        self.current_phase = Some(phase);
        let message = phase_message(phase, "starting", &self.settings);
        self.status(&message)
    }

    fn phase_finished(
        &mut self,
        phase: DiagnosticPhase,
        status: DiagnosticPhaseStatus,
        reason: Option<&EvidenceFailure>,
    ) -> io::Result<()> {
        let action = match status {
            DiagnosticPhaseStatus::Complete => "complete".to_owned(),
            DiagnosticPhaseStatus::Unavailable => reason.map_or_else(
                || "unavailable".to_owned(),
                |failure| format!("unavailable - {}", failure.message()),
            ),
        };
        let message = phase_message(phase, &action, &self.settings);
        let result = self.finish_status(&message);
        if self.current_phase == Some(phase) {
            self.current_phase = None;
        }
        result
    }
}

pub struct IndicatifDiagnosticProgress {
    progress: IndicatifProgress,
    settings: String,
    current_phase: Option<DiagnosticPhase>,
}

impl IndicatifDiagnosticProgress {
    pub fn new(color: ColorMode, settings: String) -> Self {
        Self {
            progress: IndicatifProgress::new(color),
            settings,
            current_phase: None,
        }
    }

    fn spinner(&self, message: String) {
        self.progress.spinner(message);
    }

    fn current_phase_message(&self, action: &str) -> Option<String> {
        self.current_phase
            .map(|phase| phase_message(phase, action, &self.settings))
    }
}

impl Report for IndicatifDiagnosticProgress {
    fn event(&mut self, event: BenchEvent<'_>) -> io::Result<()> {
        match event {
            BenchEvent::WarmupStart { duration, .. } => {
                if let Some(message) = self
                    .current_phase_message(&format!("warming up for {}", format_duration(duration)))
                {
                    self.spinner(message);
                }
            }
            BenchEvent::SamplingStart { samples, .. } => {
                if let Some(message) =
                    self.current_phase_message(&format!("collecting {samples} verified samples"))
                {
                    self.progress.sampling(message, samples);
                }
            }
            BenchEvent::SamplingProgress {
                completed, total, ..
            } => {
                self.progress.set_position(completed, total);
            }
            BenchEvent::AnalysisStart { .. } => {
                if let Some(message) = self.current_phase_message("analyzing") {
                    self.spinner(message);
                }
            }
            BenchEvent::TopologyPlanned { .. }
            | BenchEvent::CaseStart { .. }
            | BenchEvent::CaseComplete { .. }
            | BenchEvent::CaseSkipped { .. }
            | BenchEvent::RunComplete { .. } => {}
        }
        Ok(())
    }
}

impl P2pDiagnosticProgress for IndicatifDiagnosticProgress {
    fn phase_started(&mut self, phase: DiagnosticPhase) -> io::Result<()> {
        self.current_phase = Some(phase);
        self.spinner(phase_message(phase, "starting", &self.settings));
        Ok(())
    }

    fn phase_finished(
        &mut self,
        phase: DiagnosticPhase,
        status: DiagnosticPhaseStatus,
        reason: Option<&EvidenceFailure>,
    ) -> io::Result<()> {
        let action = match status {
            DiagnosticPhaseStatus::Complete => "complete".to_owned(),
            DiagnosticPhaseStatus::Unavailable => reason.map_or_else(
                || "unavailable".to_owned(),
                |failure| format!("unavailable - {}", failure.message()),
            ),
        };
        let message = phase_message(phase, &action, &self.settings);
        match status {
            DiagnosticPhaseStatus::Complete => self.progress.finish_complete(message),
            DiagnosticPhaseStatus::Unavailable => self.progress.finish_unavailable(message),
        }
        if self.current_phase == Some(phase) {
            self.current_phase = None;
        }
        Ok(())
    }
}

fn phase_message(phase: DiagnosticPhase, action: &str, settings: &str) -> String {
    let (index, total) = phase_position(phase);
    if phase == DiagnosticPhase::Discovery && action == "starting" && !settings.is_empty() {
        format!(
            "diag-p2p [{index}/{total}] {}: {action} ({settings})",
            phase.label()
        )
    } else {
        format!("diag-p2p [{index}/{total}] {}: {action}", phase.label())
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;

    #[test]
    fn line_progress_uses_phase_counts_and_omits_per_sample_updates() {
        let mut reporter = DiagnosticProgressReporter::new(
            Vec::new(),
            OutputFormat::Text,
            StatusMode::Line,
            "dev0 -> dev1, payload 1 GiB, 20 samples, warmup 1 s".to_owned(),
        );

        reporter
            .phase_started(DiagnosticPhase::DirectMemory)
            .expect("phase start");
        reporter
            .event(BenchEvent::SamplingStart {
                id: &crate::benchmark::CaseId::new("test".to_owned()),
                samples: 20,
                estimated: None,
            })
            .expect("sampling start");
        reporter
            .event(BenchEvent::SamplingProgress {
                id: &crate::benchmark::CaseId::new("test".to_owned()),
                completed: 10,
                total: 20,
            })
            .expect("sampling progress ignored in line mode");
        reporter
            .phase_finished(
                DiagnosticPhase::DirectMemory,
                DiagnosticPhaseStatus::Complete,
                None,
            )
            .expect("phase complete");

        let stderr = String::from_utf8(reporter.into_inner()).expect("stderr utf8");
        assert_eq!(
            stderr,
            "diag-p2p [3/8] direct memory: starting\n\
diag-p2p [3/8] direct memory: collecting 20 verified samples\n\
diag-p2p [3/8] direct memory: complete\n"
        );
    }

    #[test]
    fn settings_are_shown_once_on_discovery_start() {
        let mut reporter = DiagnosticProgressReporter::new(
            Vec::new(),
            OutputFormat::Text,
            StatusMode::Line,
            "dev0 -> dev1, payload 1 GiB, 20 samples".to_owned(),
        );

        reporter
            .phase_started(DiagnosticPhase::Discovery)
            .expect("discovery start");
        reporter
            .phase_finished(
                DiagnosticPhase::Discovery,
                DiagnosticPhaseStatus::Complete,
                None,
            )
            .expect("discovery complete");
        reporter
            .phase_started(DiagnosticPhase::StagedCalibration)
            .expect("staged start");

        let stderr = String::from_utf8(reporter.into_inner()).expect("stderr utf8");
        assert_eq!(stderr.matches("payload 1 GiB").count(), 1);
        assert!(stderr.contains("diag-p2p [2/8] staged calibration: starting\n"));
    }

    #[test]
    fn interactive_zero_length_phases_complete_without_panicking() {
        let mut reporter =
            IndicatifDiagnosticProgress::new(ColorMode::Never, "payload 1 GiB".to_owned());

        for phase in [DiagnosticPhase::Discovery, DiagnosticPhase::Acs] {
            reporter.phase_started(phase).expect("phase start");
            reporter
                .phase_finished(phase, DiagnosticPhaseStatus::Complete, None)
                .expect("phase complete");
        }
        reporter
            .phase_started(DiagnosticPhase::OptionalUpi)
            .expect("optional phase start");
        reporter
            .phase_finished(
                DiagnosticPhase::OptionalUpi,
                DiagnosticPhaseStatus::Unavailable,
                None,
            )
            .expect("optional phase unavailable");
    }

    #[test]
    fn csv_progress_is_completely_silent() {
        let mut reporter = DiagnosticProgressReporter::new(
            Vec::new(),
            OutputFormat::Csv,
            StatusMode::Interactive,
            "payload 1 GiB".to_owned(),
        );

        reporter
            .phase_started(DiagnosticPhase::Discovery)
            .expect("phase start");
        reporter
            .event(BenchEvent::SamplingProgress {
                id: &crate::benchmark::CaseId::new("test".to_owned()),
                completed: 1,
                total: 20,
            })
            .expect("progress");
        reporter
            .phase_finished(
                DiagnosticPhase::Discovery,
                DiagnosticPhaseStatus::Complete,
                None,
            )
            .expect("phase complete");

        assert!(reporter.into_inner().is_empty());
    }

    #[test]
    fn progress_broken_pipe_disables_further_status() {
        let mut reporter = DiagnosticProgressReporter::new(
            BrokenPipeWriter,
            OutputFormat::Text,
            StatusMode::Line,
            String::new(),
        );

        reporter
            .phase_started(DiagnosticPhase::Discovery)
            .expect("broken pipe should disable progress");
        reporter
            .phase_finished(
                DiagnosticPhase::Discovery,
                DiagnosticPhaseStatus::Complete,
                None,
            )
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
