use std::error::Error;
use std::io::{self, IsTerminal, Write};

use xefer::cli::{self, CliAction, Command, OutputFormat};
use xefer::output::{self, ColorMode, TextOptions};

fn main() {
    if let Err(error) = run() {
        eprintln!("{}: {error}", cli::PROGRAM_NAME);
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    match cli::parse_env()? {
        CliAction::Help(topic) => write_stdout(cli::help(topic))?,
        CliAction::Version => write_stdout(&format!("{}\n", cli::version()))?,
        CliAction::Command(Command::List(_)) => {
            let report = xefer::benchmark::list()?;
            write_stdout(&output::render_list(&report))?;
        }
        CliAction::Command(Command::Bench(options)) => {
            let report = xefer::benchmark::bench(&options)?;
            let rendered = output::render_bench(
                &report,
                options.format,
                &TextOptions {
                    include_histogram: options.histogram,
                    color: color_mode(options.format),
                },
            );
            write_stdout(&rendered)?;
        }
    }

    Ok(())
}

fn color_mode(format: OutputFormat) -> ColorMode {
    if format == OutputFormat::Text
        && io::stdout().is_terminal()
        && std::env::var_os("NO_COLOR").is_none()
    {
        ColorMode::Ansi
    } else {
        ColorMode::Never
    }
}

fn write_stdout(output: &str) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    stdout.write_all(output.as_bytes())
}
