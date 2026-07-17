//! Hand-written command-line parsing for xfer.
//!
//! The parser keeps all defaults and help/version text embedded in the binary.

use std::fmt;
use std::time::Duration;

pub const PROGRAM_NAME: &str = "xfer";
pub const VERSION: &str = match option_env!("CARGO_PKG_VERSION") {
    Some(version) => version,
    None => "0.0.0-dev",
};

pub const DEFAULT_SIZE_BYTES: u64 = 256 * 1024 * 1024;
pub const DEFAULT_SAMPLES: u32 = 50;
pub const MIN_SAMPLES: u32 = 10;
pub const DEFAULT_WARMUP: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CliAction {
    Command(Command),
    Help(HelpTopic),
    Version,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    List(ListOptions),
    Bench(BenchOptions),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ListOptions;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BenchOptions {
    pub device: Option<u32>,
    pub peer_device: Option<u32>,
    pub transfer_class: Option<TransferClass>,
    pub queue_ordinal: Option<u32>,
    pub size_bytes: u64,
    pub samples: u32,
    pub warmup: Duration,
    pub timing: TimingMode,
    pub format: OutputFormat,
    pub histogram: bool,
}

impl Default for BenchOptions {
    fn default() -> Self {
        Self {
            device: None,
            peer_device: None,
            transfer_class: None,
            queue_ordinal: None,
            size_bytes: DEFAULT_SIZE_BYTES,
            samples: DEFAULT_SAMPLES,
            warmup: DEFAULT_WARMUP,
            timing: TimingMode::WallClock,
            format: OutputFormat::Text,
            histogram: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransferClass {
    H2D,
    D2H,
    D2DSameDevice,
    D2DDirect,
    D2DStaged,
}

impl TransferClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::H2D => "h2d",
            Self::D2H => "d2h",
            Self::D2DSameDevice => "d2d-same-device",
            Self::D2DDirect => "d2d-direct",
            Self::D2DStaged => "d2d-staged",
        }
    }
}

impl fmt::Display for TransferClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimingMode {
    WallClock,
    DeviceTimestamps,
}

impl TimingMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WallClock => "wall-clock",
            Self::DeviceTimestamps => "device-timestamps",
        }
    }
}

impl fmt::Display for TimingMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputFormat {
    Text,
    Csv,
}

impl OutputFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Csv => "csv",
        }
    }
}

impl fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HelpTopic {
    General,
    List,
    Bench,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CliError {
    message: String,
}

impl CliError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CliError {}

pub fn parse_env() -> Result<CliAction, CliError> {
    parse_args(std::env::args().skip(1))
}

pub fn parse_args<I, S>(args: I) -> Result<CliAction, CliError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let args = args.into_iter().map(Into::into).collect::<Vec<_>>();
    parse_tokens(&args)
}

pub fn help(topic: HelpTopic) -> &'static str {
    match topic {
        HelpTopic::General => GENERAL_HELP,
        HelpTopic::List => LIST_HELP,
        HelpTopic::Bench => BENCH_HELP,
    }
}

pub fn version() -> String {
    format!("{PROGRAM_NAME} {VERSION}")
}

fn parse_tokens(args: &[String]) -> Result<CliAction, CliError> {
    let Some(first) = args.first() else {
        return Err(CliError::new(format!(
            "missing command\n\nRun '{PROGRAM_NAME} --help' for usage."
        )));
    };

    match first.as_str() {
        "-h" | "--help" => {
            require_no_extra(args, first).map(|()| CliAction::Help(HelpTopic::General))
        }
        "-V" | "--version" => require_no_extra(args, first).map(|()| CliAction::Version),
        "list" => parse_list(&args[1..]),
        "bench" => parse_bench(&args[1..]),
        value if value.starts_with('-') => Err(CliError::new(format!(
            "missing command before option '{value}'\n\nRun '{PROGRAM_NAME} --help' for usage."
        ))),
        value => Err(CliError::new(format!(
            "unknown command '{value}'\n\nRun '{PROGRAM_NAME} --help' for usage."
        ))),
    }
}

fn parse_list(args: &[String]) -> Result<CliAction, CliError> {
    match args {
        [] => Ok(CliAction::Command(Command::List(ListOptions))),
        [flag] if flag == "-h" || flag == "--help" => Ok(CliAction::Help(HelpTopic::List)),
        [flag] if flag == "-V" || flag == "--version" => Ok(CliAction::Version),
        [arg, ..] => Err(CliError::new(format!(
            "unexpected argument for 'list': '{arg}'\n\nRun '{PROGRAM_NAME} list --help' for usage."
        ))),
    }
}

fn parse_bench(args: &[String]) -> Result<CliAction, CliError> {
    let mut options = BenchOptions::default();
    let mut explicit_device_timestamps = false;
    let mut explicit_timing = None;
    let mut index = 0;

    while index < args.len() {
        let arg = &args[index];
        if arg == "-h" || arg == "--help" {
            require_no_remaining(args, index, arg)?;
            return Ok(CliAction::Help(HelpTopic::Bench));
        }
        if arg == "-V" || arg == "--version" {
            require_no_remaining(args, index, arg)?;
            return Ok(CliAction::Version);
        }

        let ParsedOption { name, value } = parse_long_option(arg)?;
        match name {
            "--device" => {
                let value = take_option_value(args, &mut index, name, value)?;
                options.device = Some(parse_u32(&value, name)?);
            }
            "--peer-device" => {
                let value = take_option_value(args, &mut index, name, value)?;
                options.peer_device = Some(parse_u32(&value, name)?);
            }
            "--class" | "--transfer-class" => {
                let value = take_option_value(args, &mut index, name, value)?;
                options.transfer_class = Some(parse_transfer_class(&value)?);
            }
            "--engine" | "--queue" | "--queue-ordinal" => {
                let value = take_option_value(args, &mut index, name, value)?;
                options.queue_ordinal = Some(parse_u32(&value, name)?);
            }
            "--size" => {
                let value = take_option_value(args, &mut index, name, value)?;
                options.size_bytes = parse_size_bytes(&value)?;
            }
            "--samples" => {
                let value = take_option_value(args, &mut index, name, value)?;
                options.samples = parse_sample_count(&value, name)?;
            }
            "--warmup" => {
                let value = take_option_value(args, &mut index, name, value)?;
                options.warmup = parse_duration(&value)?;
            }
            "--timing" => {
                let value = take_option_value(args, &mut index, name, value)?;
                explicit_timing = Some(parse_timing_mode(&value)?);
            }
            "--format" => {
                let value = take_option_value(args, &mut index, name, value)?;
                options.format = parse_output_format(&value)?;
            }
            "--no-histogram" => {
                reject_option_value(name, value)?;
                options.histogram = false;
            }
            "--device-timestamps" => {
                reject_option_value(name, value)?;
                explicit_device_timestamps = true;
            }
            _ => {
                return Err(CliError::new(format!(
                    "unknown option for 'bench': '{name}'\n\nRun '{PROGRAM_NAME} bench --help' for usage."
                )));
            }
        }

        index += 1;
    }

    if explicit_device_timestamps && explicit_timing == Some(TimingMode::WallClock) {
        return Err(CliError::new(
            "--device-timestamps conflicts with '--timing wall-clock'",
        ));
    }

    options.timing = if explicit_device_timestamps {
        TimingMode::DeviceTimestamps
    } else {
        explicit_timing.unwrap_or(TimingMode::WallClock)
    };

    Ok(CliAction::Command(Command::Bench(options)))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ParsedOption<'a> {
    name: &'a str,
    value: Option<&'a str>,
}

fn parse_long_option(arg: &str) -> Result<ParsedOption<'_>, CliError> {
    if !arg.starts_with("--") {
        return Err(CliError::new(format!(
            "unexpected positional argument: '{arg}'"
        )));
    }
    if arg == "--" {
        return Err(CliError::new("unexpected end-of-options marker"));
    }

    let (name, value) = match arg.split_once('=') {
        Some((name, value)) => (name, Some(value)),
        None => (arg, None),
    };

    if name.len() <= 2 {
        return Err(CliError::new(format!("invalid option: '{arg}'")));
    }

    Ok(ParsedOption { name, value })
}

fn take_option_value(
    args: &[String],
    index: &mut usize,
    name: &str,
    inline_value: Option<&str>,
) -> Result<String, CliError> {
    if let Some(value) = inline_value {
        if value.is_empty() {
            return Err(CliError::new(format!("option '{name}' requires a value")));
        }
        return Ok(value.to_owned());
    }

    *index += 1;
    let Some(value) = args.get(*index) else {
        return Err(CliError::new(format!("option '{name}' requires a value")));
    };
    if value.starts_with('-') {
        return Err(CliError::new(format!("option '{name}' requires a value")));
    }

    Ok(value.to_owned())
}

fn reject_option_value(name: &str, value: Option<&str>) -> Result<(), CliError> {
    if value.is_some() {
        Err(CliError::new(format!(
            "option '{name}' does not take a value"
        )))
    } else {
        Ok(())
    }
}

fn require_no_extra(args: &[String], flag: &str) -> Result<(), CliError> {
    if args.len() == 1 {
        Ok(())
    } else {
        Err(CliError::new(format!(
            "option '{flag}' does not take arguments"
        )))
    }
}

fn require_no_remaining(args: &[String], index: usize, flag: &str) -> Result<(), CliError> {
    if index + 1 == args.len() {
        Ok(())
    } else {
        Err(CliError::new(format!(
            "option '{flag}' does not take arguments"
        )))
    }
}

fn parse_u32(value: &str, option: &str) -> Result<u32, CliError> {
    value
        .parse::<u32>()
        .map_err(|_| CliError::new(format!("option '{option}' expects an unsigned integer")))
}

fn parse_sample_count(value: &str, option: &str) -> Result<u32, CliError> {
    let parsed = parse_u32(value, option)?;
    if parsed < MIN_SAMPLES {
        Err(CliError::new(format!(
            "option '{option}' must be at least {MIN_SAMPLES}"
        )))
    } else {
        Ok(parsed)
    }
}

fn parse_transfer_class(value: &str) -> Result<TransferClass, CliError> {
    match normalize_value(value).as_str() {
        "h2d" | "host-to-device" => Ok(TransferClass::H2D),
        "d2h" | "device-to-host" => Ok(TransferClass::D2H),
        "d2d-same-device" | "same-device" | "same" => Ok(TransferClass::D2DSameDevice),
        "d2d-direct" | "direct" => Ok(TransferClass::D2DDirect),
        "d2d-staged" | "explicit-staged" | "staged" => Ok(TransferClass::D2DStaged),
        _ => Err(CliError::new(format!(
            "invalid transfer class '{value}'; expected h2d, d2h, d2d-same-device, d2d-direct, or d2d-staged"
        ))),
    }
}

fn parse_timing_mode(value: &str) -> Result<TimingMode, CliError> {
    match normalize_value(value).as_str() {
        "wall-clock" | "wall" | "host" => Ok(TimingMode::WallClock),
        "device-timestamps" | "device" | "timestamps" => Ok(TimingMode::DeviceTimestamps),
        _ => Err(CliError::new(format!(
            "invalid timing mode '{value}'; expected wall-clock or device-timestamps"
        ))),
    }
}

fn parse_output_format(value: &str) -> Result<OutputFormat, CliError> {
    match normalize_value(value).as_str() {
        "text" => Ok(OutputFormat::Text),
        "csv" => Ok(OutputFormat::Csv),
        _ => Err(CliError::new(format!(
            "invalid format '{value}'; expected text or csv"
        ))),
    }
}

fn parse_size_bytes(value: &str) -> Result<u64, CliError> {
    let (digits, unit) = split_number_unit(value, "--size")?;
    let number = parse_u64(digits, "--size")?;
    if number == 0 {
        return Err(CliError::new("option '--size' must be greater than zero"));
    }

    let multiplier = match normalize_value(unit).as_str() {
        "" | "b" | "byte" | "bytes" => 1,
        "kib" | "ki" => 1024,
        "mib" | "mi" => 1024_u64.pow(2),
        "gib" | "gi" => 1024_u64.pow(3),
        "tib" | "ti" => 1024_u64.pow(4),
        "kb" => 1_000,
        "mb" => 1_000_000,
        "gb" => 1_000_000_000,
        "tb" => 1_000_000_000_000,
        _ => {
            return Err(CliError::new(format!(
                "invalid size unit '{unit}'; expected B, KiB, MiB, GiB, TiB, KB, MB, GB, or TB"
            )));
        }
    };

    number
        .checked_mul(multiplier)
        .ok_or_else(|| CliError::new("option '--size' is too large"))
}

fn parse_duration(value: &str) -> Result<Duration, CliError> {
    let (digits, unit) = split_number_unit(value, "--warmup")?;
    let number = parse_u64(digits, "--warmup")?;

    match normalize_value(unit).as_str() {
        "" | "s" | "sec" | "secs" | "second" | "seconds" => Ok(Duration::from_secs(number)),
        "ms" | "millisecond" | "milliseconds" => Ok(Duration::from_millis(number)),
        "us" | "usec" | "usecs" | "microsecond" | "microseconds" => {
            Ok(Duration::from_micros(number))
        }
        "m" | "min" | "mins" | "minute" | "minutes" => number
            .checked_mul(60)
            .map(Duration::from_secs)
            .ok_or_else(|| CliError::new("option '--warmup' is too large")),
        _ => Err(CliError::new(format!(
            "invalid warmup unit '{unit}'; expected us, ms, s, or m"
        ))),
    }
}

fn split_number_unit<'a>(value: &'a str, option: &str) -> Result<(&'a str, &'a str), CliError> {
    let digit_len = value.bytes().take_while(u8::is_ascii_digit).count();

    if digit_len == 0 {
        return Err(CliError::new(format!(
            "option '{option}' expects a non-negative integer with an optional unit"
        )));
    }

    Ok((&value[..digit_len], &value[digit_len..]))
}

fn parse_u64(value: &str, option: &str) -> Result<u64, CliError> {
    value
        .parse::<u64>()
        .map_err(|_| CliError::new(format!("option '{option}' expects an unsigned integer")))
}

fn normalize_value(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace('_', "-")
}

const GENERAL_HELP: &str = "\
xfer - measure Intel Level Zero transfer performance

Usage:
  xfer list
  xfer bench [OPTIONS]
  xfer --help
  xfer --version

Commands:
  list    Print Level Zero GPUs, engines, peer access, and PCIe links
  bench   Measure a useful transfer matrix, or a filtered subset

Options:
  -h, --help       Print help
  -V, --version    Print version
";

const LIST_HELP: &str = "\
Usage:
  xfer list

Print Level Zero GPUs, copy engines, peer-access matrix, and negotiated PCIe
link information.

Options:
  -h, --help       Print help
  -V, --version    Print version
";

const BENCH_HELP: &str = "\
Usage:
  xfer bench [OPTIONS]

With no options, xfer bench automatically runs a useful matrix for the
available Level Zero GPUs and engines. Filters narrow that matrix; they do not
silently select fallback devices, engines, timing modes, or transfer paths.

Options:
      --device N                  Select source/local device index
      --peer-device N             Select peer device index for cross-device cases
      --class CLASS               h2d, d2h, d2d-same-device, d2d-direct, d2d-staged
      --transfer-class CLASS      Alias for --class
      --engine ID                 Select the engine shown by 'xfer list'
      --queue ID                  Alias for --engine
      --size BYTES                Allocation size, e.g. 268435456, 256MiB, 1GB
      --samples N                 Sample count, minimum 10; default 50
      --warmup DURATION           Warm-up duration, e.g. 500ms, 1s; default 1s
      --timing MODE               wall-clock or device-timestamps
      --device-timestamps         Alias for --timing device-timestamps
      --format FORMAT             text or csv; default text
      --no-histogram              Omit text histogram
  -h, --help                      Print help
  -V, --version                   Print version
";

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(args: &[&str]) -> CliAction {
        parse_args(args.iter().copied()).expect("parse args")
    }

    fn parse_err(args: &[&str]) -> String {
        parse_args(args.iter().copied())
            .expect_err("parse should fail")
            .to_string()
    }

    #[test]
    fn parses_top_level_help_and_version() {
        assert_eq!(parse_ok(&["--help"]), CliAction::Help(HelpTopic::General));
        assert_eq!(parse_ok(&["-V"]), CliAction::Version);
    }

    #[test]
    fn parses_list_command_and_help() {
        assert_eq!(
            parse_ok(&["list"]),
            CliAction::Command(Command::List(ListOptions))
        );
        assert_eq!(
            parse_ok(&["list", "--help"]),
            CliAction::Help(HelpTopic::List)
        );
    }

    #[test]
    fn bench_defaults_are_self_contained() {
        assert_eq!(
            parse_ok(&["bench"]),
            CliAction::Command(Command::Bench(BenchOptions::default()))
        );
    }

    #[test]
    fn parses_bench_filters_and_output_flags() {
        let action = parse_ok(&[
            "bench",
            "--device",
            "0",
            "--peer-device=1",
            "--class",
            "d2d-direct",
            "--engine",
            "2",
            "--size",
            "256MiB",
            "--samples",
            "96",
            "--warmup",
            "500ms",
            "--device-timestamps",
            "--format",
            "csv",
            "--no-histogram",
        ]);

        assert_eq!(
            action,
            CliAction::Command(Command::Bench(BenchOptions {
                device: Some(0),
                peer_device: Some(1),
                transfer_class: Some(TransferClass::D2DDirect),
                queue_ordinal: Some(2),
                size_bytes: 256 * 1024 * 1024,
                samples: 96,
                warmup: Duration::from_millis(500),
                timing: TimingMode::DeviceTimestamps,
                format: OutputFormat::Csv,
                histogram: false,
            }))
        );
    }

    #[test]
    fn timing_option_and_alias_agree() {
        let action = parse_ok(&["bench", "--timing", "device-timestamps"]);
        let CliAction::Command(Command::Bench(options)) = action else {
            panic!("expected bench command");
        };
        assert_eq!(options.timing, TimingMode::DeviceTimestamps);

        assert!(
            parse_err(&["bench", "--timing", "wall-clock", "--device-timestamps"])
                .contains("conflicts")
        );
    }

    #[test]
    fn parses_decimal_and_binary_sizes() {
        let CliAction::Command(Command::Bench(binary)) = parse_ok(&["bench", "--size", "1GiB"])
        else {
            panic!("expected bench command");
        };
        let CliAction::Command(Command::Bench(decimal)) = parse_ok(&["bench", "--size", "1GB"])
        else {
            panic!("expected bench command");
        };

        assert_eq!(binary.size_bytes, 1024 * 1024 * 1024);
        assert_eq!(decimal.size_bytes, 1_000_000_000);
    }

    #[test]
    fn rejects_bad_values_without_fallback() {
        assert!(parse_err(&["run"]).contains("unknown command"));
        assert!(parse_err(&["bench", "--format", "json"]).contains("invalid format"));
        assert!(parse_err(&["bench", "--samples", "0"]).contains("at least 10"));
        assert!(parse_err(&["bench", "--samples", "9"]).contains("at least 10"));
        assert!(parse_err(&["bench", "--size", "4XB"]).contains("invalid size unit"));
        assert!(parse_err(&["bench", "--unknown"]).contains("unknown option"));
    }

    #[test]
    fn embedded_help_mentions_required_commands_and_flags() {
        assert!(help(HelpTopic::General).contains("xfer list"));
        assert!(help(HelpTopic::Bench).contains("--device-timestamps"));
        assert!(help(HelpTopic::Bench).contains("--format FORMAT"));
        assert!(version().starts_with("xfer "));
    }
}
