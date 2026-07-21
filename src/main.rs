use std::error::Error;
use std::io::{self, IsTerminal, Write};

use xfer::cli::{self, CliAction, Command, OutputFormat};
use xfer::output::{
    self, ColorMode, DiagnosticProgressReporter, DiagnosticTextOptions,
    IndicatifDiagnosticProgress, InteractiveReporter, LiveReporter, StatusMode, TextOptions,
    format_bytes,
};

fn main() {
    if let Err(error) = run() {
        if is_broken_pipe_error(error.as_ref()) {
            return;
        }
        eprintln!("{}: {error}", cli::PROGRAM_NAME);
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    match cli::parse_env()? {
        CliAction::Help(topic) => write_stdout(cli::help(topic))?,
        CliAction::Version => write_stdout(&format!("{}\n", cli::version()))?,
        CliAction::Command(Command::List(_)) => {
            let report = xfer::benchmark::list()?;
            write_stdout(&output::render_list(&report))?;
        }
        CliAction::Command(Command::Bench(options)) => {
            let stdout_is_terminal = io::stdout().is_terminal();
            let stderr_is_terminal = io::stderr().is_terminal();
            let text_options = TextOptions {
                include_histogram: options.histogram,
                summary_only: options.summary_only,
                color: color_mode(options.format, stdout_is_terminal),
            };
            let status_mode = status_mode(options.format, stderr_is_terminal);
            let color = text_options.color;
            if status_mode == StatusMode::Interactive {
                let reporter = LiveReporter::new(
                    io::stdout().lock(),
                    io::sink(),
                    options.format,
                    text_options,
                    StatusMode::Disabled,
                );
                let reporter = InteractiveReporter::new(reporter, color);
                let _report = xfer::benchmark::bench_with_reporter(&options, reporter)?;
            } else {
                let reporter = LiveReporter::new(
                    io::stdout().lock(),
                    io::stderr().lock(),
                    options.format,
                    text_options,
                    status_mode,
                );
                let _report = xfer::benchmark::bench_with_reporter(&options, reporter)?;
            }
        }
        CliAction::Command(Command::DiagP2p(options)) => {
            let stdout_is_terminal = io::stdout().is_terminal();
            let stderr_is_terminal = io::stderr().is_terminal();
            let mut diagnostic_options = xfer::diagnostics::P2pDiagnosticOptions::new(
                options.device,
                options.peer_device,
                options.size_bytes,
                options.samples,
                options.warmup,
            );
            if let Some(queue_group) = options.queue_group {
                diagnostic_options = diagnostic_options.with_queue_group(queue_group);
            }

            let status_mode = status_mode(options.format, stderr_is_terminal);
            let progress_color = color_mode(options.format, stderr_is_terminal);
            let progress_settings = diagnostic_settings(
                options.device,
                options.peer_device,
                options.size_bytes,
                options.samples,
            );
            let report = if status_mode == StatusMode::Interactive {
                let reporter = IndicatifDiagnosticProgress::new(progress_color, progress_settings);
                xfer::diagnostics::diag_p2p_with_reporter(&diagnostic_options, reporter)?
            } else {
                let reporter = DiagnosticProgressReporter::new(
                    io::stderr().lock(),
                    options.format,
                    status_mode,
                    progress_settings,
                );
                xfer::diagnostics::diag_p2p_with_reporter(&diagnostic_options, reporter)?
            };
            let output = match options.format {
                OutputFormat::Text => output::render_p2p_diagnostic_text_with_options(
                    &report,
                    DiagnosticTextOptions {
                        details: options.details,
                        color: color_mode(options.format, stdout_is_terminal),
                    },
                ),
                OutputFormat::Csv => output::render_p2p_diagnostic_csv(&report),
            };
            write_stdout(&output)?;
        }
    }

    Ok(())
}

fn color_mode(format: OutputFormat, stdout_is_terminal: bool) -> ColorMode {
    if format == OutputFormat::Text && stdout_is_terminal && std::env::var_os("NO_COLOR").is_none()
    {
        ColorMode::Ansi
    } else {
        ColorMode::Never
    }
}

fn status_mode(format: OutputFormat, stderr_is_terminal: bool) -> StatusMode {
    match (format, stderr_is_terminal) {
        (OutputFormat::Text, true) => StatusMode::Interactive,
        (OutputFormat::Text, false) => StatusMode::Line,
        (OutputFormat::Csv, _) => StatusMode::Disabled,
    }
}

fn diagnostic_settings(device: u32, peer_device: u32, size_bytes: u64, samples: u32) -> String {
    format!(
        "dev{device} -> dev{peer_device}, {}, {samples} samples",
        format_bytes(size_bytes)
    )
}

fn write_stdout(output: &str) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    stdout.write_all(output.as_bytes())?;
    stdout.flush()
}

fn is_broken_pipe_error(error: &(dyn Error + 'static)) -> bool {
    let mut current = Some(error);
    while let Some(error) = current {
        if error
            .downcast_ref::<io::Error>()
            .is_some_and(|error| error.kind() == io::ErrorKind::BrokenPipe)
        {
            return true;
        }
        current = error.source();
    }

    false
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::fmt;
    use std::io;

    use super::*;

    #[test]
    fn detects_direct_broken_pipe_error() {
        let error = io::Error::new(io::ErrorKind::BrokenPipe, "closed");
        assert!(is_broken_pipe_error(&error));
    }

    #[test]
    fn detects_nested_broken_pipe_error() {
        let error = NestedError(io::Error::new(io::ErrorKind::BrokenPipe, "closed"));
        assert!(is_broken_pipe_error(&error));
    }

    #[test]
    fn does_not_treat_other_io_errors_as_broken_pipe() {
        let error = io::Error::new(io::ErrorKind::PermissionDenied, "denied");
        assert!(!is_broken_pipe_error(&error));
    }

    #[derive(Debug)]
    struct NestedError(io::Error);

    impl fmt::Display for NestedError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("nested")
        }
    }

    impl Error for NestedError {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            Some(&self.0)
        }
    }
}
