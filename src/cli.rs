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

pub const DEFAULT_SIZE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub const DEFAULT_SAMPLES: u32 = 50;
pub const DIAG_DEFAULT_SIZE_BYTES: u64 = 1024 * 1024 * 1024;
pub const DIAG_DEFAULT_SAMPLES: u32 = 50;
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
    DiagP2p(DiagP2pOptions),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ListOptions;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BenchOptions {
    pub device: Option<u32>,
    pub peer_device: Option<u32>,
    pub transfer_class: Option<TransferClass>,
    pub queue_group: Option<u32>,
    pub size_bytes: u64,
    pub samples: u32,
    pub warmup: Duration,
    pub timing: TimingMode,
    pub format: OutputFormat,
    pub histogram: bool,
    pub summary_only: bool,
    pub mode: BenchMode,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagP2pOptions {
    pub device: u32,
    pub peer_device: u32,
    pub queue_group: Option<u32>,
    pub size_bytes: u64,
    pub samples: u32,
    pub warmup: Duration,
    pub format: OutputFormat,
    pub details: bool,
}

impl DiagP2pOptions {
    fn from_required_devices(device: u32, peer_device: u32) -> Self {
        Self {
            device,
            peer_device,
            queue_group: None,
            size_bytes: DIAG_DEFAULT_SIZE_BYTES,
            samples: DIAG_DEFAULT_SAMPLES,
            warmup: DEFAULT_WARMUP,
            format: OutputFormat::Text,
            details: false,
        }
    }
}

impl Default for BenchOptions {
    fn default() -> Self {
        Self {
            device: None,
            peer_device: None,
            transfer_class: Some(TransferClass::D2DDirect),
            queue_group: None,
            size_bytes: DEFAULT_SIZE_BYTES,
            samples: DEFAULT_SAMPLES,
            warmup: DEFAULT_WARMUP,
            timing: TimingMode::WallClock,
            format: OutputFormat::Text,
            histogram: true,
            summary_only: false,
            mode: BenchMode::Saturation,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BenchMode {
    Single,
    Saturation,
}

impl BenchMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::Saturation => "saturation",
        }
    }
}

impl fmt::Display for BenchMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
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
    DiagP2p,
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
        HelpTopic::DiagP2p => DIAG_P2P_HELP,
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
        "diag-p2p" => parse_diag_p2p(&args[1..]),
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

fn parse_diag_p2p(args: &[String]) -> Result<CliAction, CliError> {
    let mut state = DiagP2pParseState::default();
    let mut index = 0;

    while index < args.len() {
        let arg = &args[index];
        if arg == "-h" || arg == "--help" {
            require_no_remaining(args, index, arg)?;
            return Ok(CliAction::Help(HelpTopic::DiagP2p));
        }
        if arg == "-V" || arg == "--version" {
            require_no_remaining(args, index, arg)?;
            return Ok(CliAction::Version);
        }

        apply_diag_p2p_option(args, &mut index, parse_long_option(arg)?, &mut state)?;
        index += 1;
    }

    finalize_diag_p2p_options(&state).map(|options| CliAction::Command(Command::DiagP2p(options)))
}

#[derive(Default)]
struct DiagP2pParseState {
    device: Option<u32>,
    peer_device: Option<u32>,
    queue_group: Option<u32>,
    size_bytes: Option<u64>,
    samples: Option<u32>,
    warmup: Option<Duration>,
    format: Option<OutputFormat>,
    details: bool,
}

fn apply_diag_p2p_option(
    args: &[String],
    index: &mut usize,
    parsed: ParsedOption<'_>,
    state: &mut DiagP2pParseState,
) -> Result<(), CliError> {
    let ParsedOption { name, value } = parsed;
    match name {
        "--device" => {
            reject_duplicate(state.device.is_some(), name)?;
            let value = take_option_value(args, index, name, value)?;
            state.device = Some(parse_u32(&value, name)?);
        }
        "--peer-device" => {
            reject_duplicate(state.peer_device.is_some(), name)?;
            let value = take_option_value(args, index, name, value)?;
            state.peer_device = Some(parse_u32(&value, name)?);
        }
        "--queue-group" => {
            reject_duplicate(state.queue_group.is_some(), name)?;
            let value = take_option_value(args, index, name, value)?;
            state.queue_group = Some(parse_u32(&value, name)?);
        }
        "--size" => {
            reject_duplicate(state.size_bytes.is_some(), name)?;
            let value = take_option_value(args, index, name, value)?;
            state.size_bytes = Some(parse_size_bytes(&value)?);
        }
        "--samples" => {
            reject_duplicate(state.samples.is_some(), name)?;
            let value = take_option_value(args, index, name, value)?;
            state.samples = Some(parse_sample_count(&value, name)?);
        }
        "--warmup" => {
            reject_duplicate(state.warmup.is_some(), name)?;
            let value = take_option_value(args, index, name, value)?;
            state.warmup = Some(parse_duration(&value)?);
        }
        "--format" => {
            reject_duplicate(state.format.is_some(), name)?;
            let value = take_option_value(args, index, name, value)?;
            state.format = Some(parse_output_format(&value)?);
        }
        "--details" => {
            reject_option_value(name, value)?;
            reject_duplicate(state.details, name)?;
            state.details = true;
        }
        _ => {
            return Err(CliError::new(format!(
                "unknown option for 'diag-p2p': '{name}'\n\nRun '{PROGRAM_NAME} diag-p2p --help' for usage."
            )));
        }
    }

    Ok(())
}

fn finalize_diag_p2p_options(state: &DiagP2pParseState) -> Result<DiagP2pOptions, CliError> {
    let device = state.device.ok_or_else(|| {
        CliError::new(format!(
            "missing required option '--device'\n\nRun '{PROGRAM_NAME} diag-p2p --help' for usage."
        ))
    })?;
    let peer_device = state.peer_device.ok_or_else(|| {
        CliError::new(format!(
            "missing required option '--peer-device'\n\nRun '{PROGRAM_NAME} diag-p2p --help' for usage."
        ))
    })?;
    if device == peer_device {
        return Err(CliError::new(
            "'--device' and '--peer-device' must be distinct for 'diag-p2p'",
        ));
    }

    let mut options = DiagP2pOptions::from_required_devices(device, peer_device);
    options.queue_group = state.queue_group;
    options.size_bytes = state.size_bytes.unwrap_or(DIAG_DEFAULT_SIZE_BYTES);
    options.samples = state.samples.unwrap_or(DIAG_DEFAULT_SAMPLES);
    options.warmup = state.warmup.unwrap_or(DEFAULT_WARMUP);
    options.format = state.format.unwrap_or(OutputFormat::Text);
    options.details = state.details;
    if options.details && options.format == OutputFormat::Csv {
        return Err(CliError::new(
            "'--details' requires '--format text' for 'diag-p2p'",
        ));
    }
    Ok(options)
}

fn reject_duplicate(already_present: bool, name: &str) -> Result<(), CliError> {
    if already_present {
        Err(CliError::new(format!(
            "option '{name}' was provided more than once"
        )))
    } else {
        Ok(())
    }
}

fn parse_bench(args: &[String]) -> Result<CliAction, CliError> {
    let mut state = BenchParseState {
        options: BenchOptions::default(),
        explicit_timing: None,
        explicit_mode: None,
    };
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
        if arg == "-s" {
            set_bench_mode(&mut state, BenchMode::Saturation)?;
            index += 1;
            continue;
        }

        apply_bench_option(args, &mut index, parse_long_option(arg)?, &mut state)?;
        index += 1;
    }

    finalize_bench_options(state).map(|options| CliAction::Command(Command::Bench(options)))
}

struct BenchParseState {
    options: BenchOptions,
    explicit_timing: Option<TimingMode>,
    explicit_mode: Option<BenchMode>,
}

fn apply_bench_option(
    args: &[String],
    index: &mut usize,
    parsed: ParsedOption<'_>,
    state: &mut BenchParseState,
) -> Result<(), CliError> {
    let ParsedOption { name, value } = parsed;
    match name {
        "--device" => {
            let value = take_option_value(args, index, name, value)?;
            state.options.device = Some(parse_u32(&value, name)?);
        }
        "--peer-device" => {
            let value = take_option_value(args, index, name, value)?;
            state.options.peer_device = Some(parse_u32(&value, name)?);
        }
        "--class" => {
            let value = take_option_value(args, index, name, value)?;
            state.options.transfer_class = parse_transfer_class(&value)?;
        }
        "--queue-group" => {
            let value = take_option_value(args, index, name, value)?;
            state.options.queue_group = Some(parse_u32(&value, name)?);
        }
        "--size" => {
            let value = take_option_value(args, index, name, value)?;
            state.options.size_bytes = parse_size_bytes(&value)?;
        }
        "--samples" => {
            let value = take_option_value(args, index, name, value)?;
            state.options.samples = parse_sample_count(&value, name)?;
        }
        "--warmup" => {
            let value = take_option_value(args, index, name, value)?;
            state.options.warmup = parse_duration(&value)?;
        }
        "--timing" => {
            let value = take_option_value(args, index, name, value)?;
            state.explicit_timing = Some(parse_timing_mode(&value)?);
        }
        "--format" => {
            let value = take_option_value(args, index, name, value)?;
            state.options.format = parse_output_format(&value)?;
        }
        "--no-histogram" => {
            reject_option_value(name, value)?;
            state.options.histogram = false;
        }
        "--summary-only" => {
            reject_option_value(name, value)?;
            state.options.summary_only = true;
        }
        "--saturation" => {
            reject_option_value(name, value)?;
            set_bench_mode(state, BenchMode::Saturation)?;
        }
        "--single" => {
            reject_option_value(name, value)?;
            set_bench_mode(state, BenchMode::Single)?;
        }
        _ => {
            return Err(CliError::new(format!(
                "unknown option for 'bench': '{name}'\n\nRun '{PROGRAM_NAME} bench --help' for usage."
            )));
        }
    }
    Ok(())
}

fn finalize_bench_options(mut state: BenchParseState) -> Result<BenchOptions, CliError> {
    state.options.timing = state.explicit_timing.unwrap_or(TimingMode::WallClock);
    if state.options.mode == BenchMode::Saturation
        && state.options.timing == TimingMode::DeviceTimestamps
    {
        return Err(CliError::new(
            "saturation mode supports wall-clock timing only; cross-queue device timestamps do not form one aggregate interval",
        ));
    }
    if state.options.summary_only && state.options.format == OutputFormat::Csv {
        return Err(CliError::new(
            "--summary-only requires '--format text'; CSV already emits one stable row per case",
        ));
    }

    Ok(state.options)
}

fn set_bench_mode(state: &mut BenchParseState, mode: BenchMode) -> Result<(), CliError> {
    if state
        .explicit_mode
        .is_some_and(|explicit_mode| explicit_mode != mode)
    {
        return Err(CliError::new(
            "--single conflicts with '--saturation' and '-s'",
        ));
    }
    state.explicit_mode = Some(mode);
    state.options.mode = mode;
    Ok(())
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

fn parse_transfer_class(value: &str) -> Result<Option<TransferClass>, CliError> {
    match normalize_value(value).as_str() {
        "all" => Ok(None),
        "h2d" | "host-to-device" => Ok(Some(TransferClass::H2D)),
        "d2h" | "device-to-host" => Ok(Some(TransferClass::D2H)),
        "d2d-same-device" | "same-device" | "same" => Ok(Some(TransferClass::D2DSameDevice)),
        "d2d-direct" | "direct" => Ok(Some(TransferClass::D2DDirect)),
        "d2d-staged" | "explicit-staged" | "staged" => Ok(Some(TransferClass::D2DStaged)),
        _ => Err(CliError::new(format!(
            "invalid transfer class '{value}'; expected all, h2d, d2h, d2d-same-device, d2d-direct, or d2d-staged"
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
  xfer diag-p2p --device N --peer-device N [OPTIONS]
  xfer --help
  xfer --version

Commands:
  list      Show GPUs, queue groups, peer access, and PCIe routes
  bench     Measure sustained Level Zero direct GPU-memory copy request bandwidth
  diag-p2p  Diagnose peer-counter vs host-memory-counter evidence

Common examples:
  xfer bench
  xfer bench --summary-only
  xfer bench --device 0 --peer-device 1
  xfer diag-p2p --device 0 --peer-device 1

Options:
  -h, --help       Print help
  -V, --version    Print version
";

const LIST_HELP: &str = "\
Usage:
  xfer list

Show Level Zero GPUs, command queue groups, device-to-device access, and
negotiated PCIe links and routes.

Options:
  -h, --help       Print help
  -V, --version    Print version
";

const BENCH_HELP: &str = "\
Usage:
  xfer bench [OPTIONS]

With no options, xfer measures Level Zero direct GPU-memory copy request
saturation bandwidth for every ordered device pair. Defaults: all copy queues,
2 GiB, 50 samples, 1 s warm-up.

Common options:
      --summary-only              Print only the final report
      --format FORMAT             text or csv; default text
      --no-histogram              Omit per-test histograms
  -s, --saturation               Use all selected copy queues; default

Advanced filters:
      --device N                  Select source/local device index
      --peer-device N             Select peer device index for cross-device cases
      --class CLASS               direct, staged, same, h2d, d2h, or all
      --queue-group ID            Select the queue group shown by 'xfer list'
      --single                    Test each selected queue group separately
      --size BYTES                Payload size, e.g. 2GiB, 2048MiB; default 2GiB
      --samples N                 Sample count, minimum 10; default 50
      --warmup DURATION           Warm-up duration, e.g. 500ms, 1s; default 1s
      --timing MODE               wall-clock or device-timestamps

  -h, --help                      Print help
  -V, --version                   Print version
";

const DIAG_P2P_HELP: &str = "\
Usage:
  xfer diag-p2p --device N --peer-device N [OPTIONS]

Diagnose whether a Level Zero direct GPU-memory copy request is more consistent
with peer traffic or explicit host staging. The direct request and peer-access
API capability are evidence inputs, not proof of the physical data route.
diag-p2p calibrates explicit host staging, then checks direct-copy IIO memory
and peer counters plus live PCIe topology, endpoint links, and ACS policy.
Its strongest result is counter-consistent evidence; it cannot prove a precise
physical packet route without transaction-tagged telemetry or external tracing.

Options:
      --device N                  Source device index shown by 'xfer list'
      --peer-device N             Destination peer device index; must differ
      --queue-group ID            Select the queue group shown by 'xfer list'
      --size BYTES                Payload size, e.g. 1GiB, 512MiB; default 1GiB
      --samples N                 Sample count, minimum 10; default 50
      --warmup DURATION           Warm-up duration, e.g. 500ms, 1s; default 1s
      --format FORMAT             text or csv; default text
      --details                   Include raw gate reasons, counter roles, and bridge rows; text only

Strongest counter and ACS evidence may require rerunning the absolute release
binary with sudo, for example:
  sudo /absolute/path/to/xfer diag-p2p --device N --peer-device N

Without access, ACS and counter evidence are reported unavailable and the
verdict degrades. sudo is not a remedy for loader startup or environment setup
failures.

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
        let CliAction::Command(Command::Bench(options)) = parse_ok(&["bench"]) else {
            panic!("expected bench command");
        };

        assert_eq!(options, BenchOptions::default());
        assert_eq!(options.transfer_class, Some(TransferClass::D2DDirect));
        assert_eq!(options.mode, BenchMode::Saturation);
        assert_eq!(options.size_bytes, 2 * 1024 * 1024 * 1024);
        assert_eq!(options.samples, 50);
        assert_eq!(options.warmup, Duration::from_secs(1));
    }

    #[test]
    fn diagnostic_defaults_are_separate_named_values() {
        assert_eq!(DEFAULT_SIZE_BYTES, 2 * 1024 * 1024 * 1024);
        assert_eq!(DEFAULT_SAMPLES, 50);
        assert_eq!(DIAG_DEFAULT_SIZE_BYTES, 1024 * 1024 * 1024);
        assert_eq!(DIAG_DEFAULT_SAMPLES, 50);
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
            "--queue-group",
            "2",
            "--size",
            "256MiB",
            "--samples",
            "96",
            "--warmup",
            "500ms",
            "--single",
            "--timing",
            "device-timestamps",
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
                queue_group: Some(2),
                size_bytes: 256 * 1024 * 1024,
                samples: 96,
                warmup: Duration::from_millis(500),
                timing: TimingMode::DeviceTimestamps,
                format: OutputFormat::Csv,
                histogram: false,
                summary_only: false,
                mode: BenchMode::Single,
            }))
        );
    }

    #[test]
    fn parses_diag_p2p_defaults() {
        let action = parse_ok(&["diag-p2p", "--device", "0", "--peer-device", "1"]);

        assert_eq!(
            action,
            CliAction::Command(Command::DiagP2p(DiagP2pOptions {
                device: 0,
                peer_device: 1,
                queue_group: None,
                size_bytes: DIAG_DEFAULT_SIZE_BYTES,
                samples: DIAG_DEFAULT_SAMPLES,
                warmup: DEFAULT_WARMUP,
                format: OutputFormat::Text,
                details: false,
            }))
        );
    }

    #[test]
    fn parses_diag_p2p_options() {
        let action = parse_ok(&[
            "diag-p2p",
            "--device=2",
            "--peer-device",
            "0",
            "--queue-group",
            "3",
            "--size",
            "512MiB",
            "--samples",
            "64",
            "--warmup",
            "750ms",
            "--details",
        ]);

        assert_eq!(
            action,
            CliAction::Command(Command::DiagP2p(DiagP2pOptions {
                device: 2,
                peer_device: 0,
                queue_group: Some(3),
                size_bytes: 512 * 1024 * 1024,
                samples: 64,
                warmup: Duration::from_millis(750),
                format: OutputFormat::Text,
                details: true,
            }))
        );
    }

    #[test]
    fn diag_p2p_details_is_text_only() {
        for args in [
            [
                "diag-p2p",
                "--device",
                "0",
                "--peer-device",
                "1",
                "--details",
                "--format=csv",
            ],
            [
                "diag-p2p",
                "--device",
                "0",
                "--peer-device",
                "1",
                "--format=csv",
                "--details",
            ],
        ] {
            assert!(parse_err(&args).contains("requires '--format text'"));
        }
    }

    #[test]
    fn diag_p2p_requires_both_distinct_devices() {
        assert!(parse_err(&["diag-p2p", "--peer-device", "1"]).contains("--device"));
        assert!(parse_err(&["diag-p2p", "--device", "0"]).contains("--peer-device"));
        assert!(
            parse_err(&["diag-p2p", "--device", "1", "--peer-device", "1"]).contains("distinct")
        );
    }

    #[test]
    fn diag_p2p_rejects_duplicates_and_bad_values() {
        assert!(
            parse_err(&[
                "diag-p2p",
                "--device",
                "0",
                "--device",
                "1",
                "--peer-device",
                "2",
            ])
            .contains("more than once")
        );
        assert!(
            parse_err(&[
                "diag-p2p",
                "--device",
                "0",
                "--peer-device",
                "1",
                "--queue-group",
                "x",
            ])
            .contains("unsigned integer")
        );
        assert!(
            parse_err(&[
                "diag-p2p",
                "--device",
                "0",
                "--peer-device",
                "1",
                "--size",
                "4XB",
            ])
            .contains("invalid size unit")
        );
        assert!(
            parse_err(&[
                "diag-p2p",
                "--device",
                "0",
                "--peer-device",
                "1",
                "--samples",
                "9",
            ])
            .contains("at least 10")
        );
        assert!(
            parse_err(&[
                "diag-p2p",
                "--device",
                "0",
                "--peer-device",
                "1",
                "--format",
                "json",
            ])
            .contains("invalid format")
        );
    }

    #[test]
    fn diag_p2p_accepts_help_and_version_only_as_terminal_flags() {
        assert_eq!(
            parse_ok(&["diag-p2p", "--help"]),
            CliAction::Help(HelpTopic::DiagP2p)
        );
        assert_eq!(parse_ok(&["diag-p2p", "--version"]), CliAction::Version);
        assert!(
            parse_err(&["diag-p2p", "--help", "--device", "0"]).contains("does not take arguments")
        );
    }

    #[test]
    fn diag_p2p_has_exact_public_command_spelling() {
        assert!(
            parse_ok(&["diag-p2p", "--device", "0", "--peer-device", "1"]).eq(&CliAction::Command(
                Command::DiagP2p(DiagP2pOptions::from_required_devices(0, 1),)
            ))
        );
        assert!(parse_err(&["diagnose-p2p"]).contains("unknown command"));
        assert!(parse_err(&["diag-p2p", "--class", "all"]).contains("unknown option"));
        assert!(parse_err(&["diag-p2p", "--single"]).contains("unknown option"));
        assert!(parse_err(&["diag-p2p", "--timing", "wall-clock"]).contains("unknown option"));
        assert!(parse_err(&["diag-p2p", "--no-histogram"]).contains("unknown option"));
    }

    #[test]
    fn timing_option_selects_device_timestamps() {
        let action = parse_ok(&["bench", "--single", "--timing", "device-timestamps"]);
        let CliAction::Command(Command::Bench(options)) = action else {
            panic!("expected bench command");
        };
        assert_eq!(options.timing, TimingMode::DeviceTimestamps);
    }

    #[test]
    fn saturation_aliases_and_timing_conflict_are_explicit() {
        let CliAction::Command(Command::Bench(defaults)) = parse_ok(&["bench"]) else {
            panic!("expected bench command");
        };
        assert_eq!(defaults.mode, BenchMode::Saturation);

        for flag in ["-s", "--saturation"] {
            let CliAction::Command(Command::Bench(options)) = parse_ok(&["bench", flag]) else {
                panic!("expected bench command");
            };
            assert_eq!(options.mode, BenchMode::Saturation);
        }

        let CliAction::Command(Command::Bench(single)) = parse_ok(&["bench", "--single"]) else {
            panic!("expected bench command");
        };
        assert_eq!(single.mode, BenchMode::Single);

        assert!(parse_err(&["bench", "--single", "--saturation"]).contains("conflicts"));
        assert!(parse_err(&["bench", "-s", "--single"]).contains("conflicts"));
        assert!(
            parse_err(&["bench", "--timing", "device-timestamps"])
                .contains("supports wall-clock timing only")
        );
    }

    #[test]
    fn class_all_selects_the_full_transfer_matrix() {
        let CliAction::Command(Command::Bench(options)) = parse_ok(&["bench", "--class", "all"])
        else {
            panic!("expected bench command");
        };

        assert_eq!(options.transfer_class, None);
    }

    #[test]
    fn every_documented_transfer_class_is_exposed() {
        for (name, expected) in [
            ("h2d", Some(TransferClass::H2D)),
            ("d2h", Some(TransferClass::D2H)),
            ("same", Some(TransferClass::D2DSameDevice)),
            ("direct", Some(TransferClass::D2DDirect)),
            ("staged", Some(TransferClass::D2DStaged)),
            ("all", None),
        ] {
            let CliAction::Command(Command::Bench(options)) = parse_ok(&["bench", "--class", name])
            else {
                panic!("expected bench command");
            };
            assert_eq!(options.transfer_class, expected);
        }
    }

    #[test]
    fn summary_only_is_text_only() {
        let CliAction::Command(Command::Bench(options)) = parse_ok(&["bench", "--summary-only"])
        else {
            panic!("expected bench command");
        };
        assert!(options.summary_only);
        assert!(
            parse_err(&["bench", "--summary-only", "--format", "csv"])
                .contains("requires '--format text'")
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
        assert!(parse_err(&["measure"]).contains("unknown command"));
        assert!(parse_err(&["bench", "--format", "json"]).contains("invalid format"));
        assert!(parse_err(&["bench", "--samples", "0"]).contains("at least 10"));
        assert!(parse_err(&["bench", "--samples", "9"]).contains("at least 10"));
        assert!(parse_err(&["bench", "--size", "4XB"]).contains("invalid size unit"));
        assert!(parse_err(&["bench", "--unknown"]).contains("unknown option"));
        for removed in [
            "--engine",
            "--queue",
            "--queue-ordinal",
            "--transfer-class",
            "--device-timestamps",
        ] {
            assert!(parse_err(&["bench", removed]).contains("unknown option"));
        }
    }

    #[test]
    fn embedded_help_mentions_required_commands_and_flags() {
        assert!(help(HelpTopic::General).contains("xfer list"));
        assert!(help(HelpTopic::General).contains("xfer bench"));
        assert!(help(HelpTopic::General).contains("xfer diag-p2p"));
        assert!(help(HelpTopic::General).contains("host-memory-counter evidence"));
        assert!(!help(HelpTopic::General).contains("direct GPU-to-GPU"));
        assert!(help(HelpTopic::Bench).contains("device-timestamps"));
        assert!(help(HelpTopic::Bench).contains("--format FORMAT"));
        assert!(help(HelpTopic::Bench).contains("--summary-only"));
        assert!(help(HelpTopic::Bench).contains("--single"));
        assert!(help(HelpTopic::Bench).contains("--queue-group ID"));
        assert!(!help(HelpTopic::Bench).contains("direct GPU-to-GPU"));
        assert!(!help(HelpTopic::Bench).contains("--engine"));
        assert!(help(HelpTopic::Bench).contains("default 2GiB"));
        assert!(help(HelpTopic::DiagP2p).contains("sudo /absolute/path/to/xfer diag-p2p"));
        assert!(help(HelpTopic::DiagP2p).contains("not proof of the physical data route"));
        assert!(help(HelpTopic::DiagP2p).contains("default 1GiB"));
        assert!(help(HelpTopic::DiagP2p).contains("default 50"));
        assert!(help(HelpTopic::DiagP2p).contains("--details"));
        assert!(!help(HelpTopic::DiagP2p).contains("--single"));
        assert!(!help(HelpTopic::DiagP2p).contains("--timing"));
        assert!(version().starts_with("xfer "));
    }
}
