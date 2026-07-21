use std::io::{self, Write};
use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};

use super::{ColorMode, StatusMode};

const TICK_INTERVAL: Duration = Duration::from_millis(80);

struct IndicatifStyles {
    spinner: ProgressStyle,
    sampling: ProgressStyle,
    complete: ProgressStyle,
    unavailable: ProgressStyle,
}

pub(super) struct IndicatifProgress {
    bar: ProgressBar,
    styles: IndicatifStyles,
}

impl IndicatifProgress {
    pub(super) fn new(color: ColorMode) -> Self {
        let styles = indicatif_styles(color);
        let bar = ProgressBar::new(0);
        bar.set_style(styles.spinner.clone());
        bar.enable_steady_tick(TICK_INTERVAL);
        Self { bar, styles }
    }

    pub(super) fn spinner(&self, message: String) {
        self.bar.reset();
        self.bar.set_style(self.styles.spinner.clone());
        self.bar.set_message(message);
        self.bar.enable_steady_tick(TICK_INTERVAL);
    }

    pub(super) fn sampling(&self, message: String, samples: u32) {
        self.bar.disable_steady_tick();
        self.bar.reset();
        self.bar.set_length(u64::from(samples));
        self.bar.set_position(0);
        self.bar.set_style(self.styles.sampling.clone());
        self.bar.set_message(message);
    }

    pub(super) fn set_position(&self, completed: u32, total: u32) {
        self.bar.set_length(u64::from(total));
        self.bar.set_position(u64::from(completed));
    }

    pub(super) fn clear(&self) {
        self.bar.finish_and_clear();
    }

    pub(super) fn finish_complete(&self, message: String) {
        self.finish(message, &self.styles.complete);
    }

    pub(super) fn finish_unavailable(&self, message: String) {
        self.finish(message, &self.styles.unavailable);
    }

    fn finish(&self, message: String, style: &ProgressStyle) {
        self.bar.reset();
        self.bar.set_style(style.clone());
        self.bar.finish_with_message(message);
    }
}

impl Drop for IndicatifProgress {
    fn drop(&mut self) {
        self.bar.finish_and_clear();
    }
}

pub(super) fn ascii_progress_bar(completed: u32, total: u32) -> String {
    const WIDTH: usize = 20;
    let filled = if total == 0 {
        0
    } else {
        (completed.min(total) as usize * WIDTH) / total as usize
    };
    format!("[{}{}]", "=".repeat(filled), " ".repeat(WIDTH - filled))
}

fn indicatif_styles(color: ColorMode) -> IndicatifStyles {
    let (spinner_template, sampling_template, complete_template, unavailable_template) = match color
    {
        ColorMode::Ansi => (
            "{spinner:.cyan} {msg}",
            "{spinner:.cyan} {msg} [{bar:20.cyan/bright_black}] {pos}/{len}",
            "{spinner:.green} {msg}",
            "{spinner:.yellow} {msg}",
        ),
        ColorMode::Never => (
            "{spinner} {msg}",
            "{spinner} {msg} [{bar:20}] {pos}/{len}",
            "{spinner} {msg}",
            "{spinner} {msg}",
        ),
    };

    IndicatifStyles {
        spinner: ProgressStyle::with_template(spinner_template)
            .expect("static spinner template is valid")
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
        sampling: ProgressStyle::with_template(sampling_template)
            .expect("static sampling template is valid")
            .progress_chars("█▉▊▋▌▍▎▏  "),
        complete: ProgressStyle::with_template(complete_template)
            .expect("static completion template is valid")
            .tick_strings(&["✓", "✓"]),
        unavailable: ProgressStyle::with_template(unavailable_template)
            .expect("static unavailable template is valid")
            .tick_strings(&["!", "!"]),
    }
}

pub(super) fn write_status<W: Write>(
    writer: &mut W,
    mode: StatusMode,
    enabled: &mut bool,
    message: &str,
    finish_line: bool,
) -> io::Result<()> {
    if !*enabled || mode == StatusMode::Disabled {
        return Ok(());
    }

    let result = match mode {
        StatusMode::Interactive => write!(writer, "\r\u{1b}[2K{message}")
            .and_then(|()| {
                if finish_line {
                    writer.write_all(b"\n")
                } else {
                    Ok(())
                }
            })
            .and_then(|()| writer.flush()),
        StatusMode::Line => writeln!(writer, "{message}").and_then(|()| writer.flush()),
        StatusMode::Disabled => Ok(()),
    };

    handle_status_result(result, enabled)
}

pub(super) fn clear_status<W: Write>(writer: &mut W, enabled: &mut bool) -> io::Result<()> {
    if !*enabled {
        return Ok(());
    }

    let result = write!(writer, "\r\u{1b}[2K").and_then(|()| writer.flush());
    handle_status_result(result, enabled)
}

fn handle_status_result(result: io::Result<()>, enabled: &mut bool) -> io::Result<()> {
    match result {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {
            *enabled = false;
            Ok(())
        }
        Err(error) => Err(error),
    }
}
