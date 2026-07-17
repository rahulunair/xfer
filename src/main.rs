use std::error::Error;
use std::io::{self, IsTerminal, Write};

use xfer::cli::{self, CliAction, Command, OutputFormat};
use xfer::output::{self, ColorMode, LiveReporter, StatusMode, TextOptions};

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
                color: color_mode(options.format, stdout_is_terminal),
            };
            let status_mode = status_mode(options.format, stderr_is_terminal);
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
