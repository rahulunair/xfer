use std::error::Error as StdError;
use std::fmt;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use super::cpu::CpuIdentity;
use super::intel_perfmon::{EventRole, PerfmonProfileId};
use super::linux_pmu::{FormatFieldName, PmuKind};

pub type Result<T> = std::result::Result<T, EvidenceError>;

#[derive(Debug)]
pub enum EvidenceError {
    UnsupportedCpu(CpuIdentity),
    MissingPmu {
        kind: PmuKind,
    },
    PermissionDenied {
        path: PathBuf,
        source: io::Error,
    },
    MalformedSysfs {
        path: PathBuf,
        reason: String,
    },
    UnavailableEvent {
        profile: PerfmonProfileId,
        role: EventRole,
        reason: String,
    },
    InvalidEventEncoding {
        reason: String,
    },
    MissingFormatField {
        field: FormatFieldName,
    },
    FieldValueTooLarge {
        field: FormatFieldName,
        value: u64,
        width: u8,
    },
    PerfEvent(PerfEventFailure),
    Counter(CounterFailure),
    Io {
        path: PathBuf,
        source: io::Error,
    },
}

#[derive(Debug)]
pub enum PerfEventFailure {
    PermissionDenied {
        context: String,
        source: io::Error,
    },
    Unsupported {
        context: String,
        source: io::Error,
    },
    ResourceUnavailable {
        context: String,
        source: PerfEventResourceFailure,
    },
    RejectedAttr {
        context: String,
        source: io::Error,
    },
    Malformed {
        context: String,
        reason: String,
    },
    Io {
        context: String,
        source: io::Error,
    },
    Multiple {
        context: String,
        failures: Vec<EvidenceError>,
    },
}

#[derive(Debug)]
pub enum PerfEventResourceFailure {
    System(io::Error),
    Closed { reason: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CounterFailure {
    RunMismatch,
    TimeOrder,
    CounterSetMismatch,
    DuplicateCounterId {
        id: String,
    },
    RoleChanged {
        id: String,
        before: String,
        after: String,
    },
    ValueRegression {
        id: String,
    },
    TimeEnabledRegression {
        id: String,
    },
    TimeRunningRegression {
        id: String,
    },
    InvalidTiming {
        context: CounterTimingContext,
        time_enabled: Duration,
        time_running: Duration,
    },
    ZeroRunning {
        id: String,
    },
    MultiplexingRefused {
        id: String,
    },
    SyntheticSourceExhausted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CounterTimingContext {
    Reading,
    Delta,
}

impl fmt::Display for EvidenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedCpu(cpu) => {
                write!(f, "unsupported CPU for hardware-counter evidence: {cpu}")
            }
            Self::MissingPmu { kind } => write!(f, "missing Linux PMU for {kind}"),
            Self::PermissionDenied { path, source } => {
                write!(f, "permission denied reading {}: {source}", path.display())
            }
            Self::MalformedSysfs { path, reason } => {
                write!(
                    f,
                    "malformed sysfs PMU data in {}: {reason}",
                    path.display()
                )
            }
            Self::UnavailableEvent {
                profile,
                role,
                reason,
            } => write!(
                f,
                "event {role} is unavailable for perfmon profile {profile}: {reason}"
            ),
            Self::InvalidEventEncoding { reason } => {
                write!(f, "invalid perfmon event encoding: {reason}")
            }
            Self::MissingFormatField { field } => {
                write!(f, "Linux PMU format is missing required field {field}")
            }
            Self::FieldValueTooLarge {
                field,
                value,
                width,
            } => write!(
                f,
                "value 0x{value:x} for Linux PMU field {field} does not fit in {width} bits"
            ),
            Self::PerfEvent(reason) => write!(f, "{reason}"),
            Self::Counter(reason) => write!(f, "{reason}"),
            Self::Io { path, source } => write!(f, "cannot read {}: {source}", path.display()),
        }
    }
}

impl fmt::Display for PerfEventFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PermissionDenied { context, source } => {
                write!(
                    f,
                    "permission denied for Linux perf event {context}: {source}"
                )
            }
            Self::Unsupported { context, source } => {
                write!(
                    f,
                    "unsupported Linux perf event operation {context}: {source}"
                )
            }
            Self::ResourceUnavailable { context, source } => {
                write!(
                    f,
                    "Linux perf event resource unavailable for {context}: {source}"
                )
            }
            Self::RejectedAttr { context, source } => {
                write!(
                    f,
                    "Linux kernel rejected perf_event_open attr/encoding for {context}: {source}"
                )
            }
            Self::Malformed { context, reason } => {
                write!(f, "malformed Linux perf event data for {context}: {reason}")
            }
            Self::Io { context, source } => {
                write!(f, "Linux perf event I/O failed for {context}: {source}")
            }
            Self::Multiple { context, failures } => {
                write!(f, "multiple Linux perf event failures during {context}: ")?;
                for (index, failure) in failures.iter().enumerate() {
                    if index > 0 {
                        f.write_str("; ")?;
                    }
                    write!(f, "{failure}")?;
                }
                Ok(())
            }
        }
    }
}

impl fmt::Display for PerfEventResourceFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::System(source) => write!(f, "{source}"),
            Self::Closed { reason } => write!(f, "counter set is closed: {reason}"),
        }
    }
}

impl fmt::Display for CounterFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RunMismatch => f.write_str("counter snapshots belong to different evidence runs"),
            Self::TimeOrder => f.write_str("counter snapshots are out of order"),
            Self::CounterSetMismatch => {
                f.write_str("counter snapshots do not contain the same counter ids")
            }
            Self::DuplicateCounterId { id } => write!(f, "duplicate counter id {id} in snapshot"),
            Self::RoleChanged { id, before, after } => {
                write!(f, "{id} changed role from {before} to {after}")
            }
            Self::ValueRegression { id } => write!(f, "{id} counter value went backwards"),
            Self::TimeEnabledRegression { id } => write!(f, "{id} time_enabled went backwards"),
            Self::TimeRunningRegression { id } => write!(f, "{id} time_running went backwards"),
            Self::InvalidTiming {
                context,
                time_enabled,
                time_running,
            } => write!(
                f,
                "{context} has time_running {time_running:?} greater than time_enabled {time_enabled:?}"
            ),
            Self::ZeroRunning { id } => {
                write!(f, "{id} cannot be attributed because time_running is zero")
            }
            Self::MultiplexingRefused { id } => {
                write!(
                    f,
                    "{id} was multiplexed; acknowledge scaling before attribution"
                )
            }
            Self::SyntheticSourceExhausted => {
                f.write_str("synthetic counter source has no remaining snapshots")
            }
        }
    }
}

impl fmt::Display for CounterTimingContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Reading => f.write_str("counter reading"),
            Self::Delta => f.write_str("counter delta"),
        }
    }
}

impl StdError for EvidenceError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::PermissionDenied { source, .. } | Self::Io { source, .. } => Some(source),
            Self::PerfEvent(reason) => match reason {
                PerfEventFailure::PermissionDenied { source, .. }
                | PerfEventFailure::Unsupported { source, .. }
                | PerfEventFailure::RejectedAttr { source, .. }
                | PerfEventFailure::Io { source, .. } => Some(source),
                PerfEventFailure::ResourceUnavailable { source, .. } => Some(source),
                PerfEventFailure::Multiple { failures, .. } => failures
                    .first()
                    .map(|failure| failure as &(dyn StdError + 'static)),
                PerfEventFailure::Malformed { .. } => None,
            },
            Self::UnsupportedCpu(_)
            | Self::MissingPmu { .. }
            | Self::MalformedSysfs { .. }
            | Self::UnavailableEvent { .. }
            | Self::InvalidEventEncoding { .. }
            | Self::MissingFormatField { .. }
            | Self::FieldValueTooLarge { .. }
            | Self::Counter(_) => None,
        }
    }
}

impl StdError for PerfEventResourceFailure {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::System(source) => Some(source),
            Self::Closed { .. } => None,
        }
    }
}

pub(crate) fn read_error(path: impl Into<PathBuf>, error: io::Error) -> EvidenceError {
    let path = path.into();
    if error.kind() == io::ErrorKind::PermissionDenied {
        EvidenceError::PermissionDenied {
            path,
            source: error,
        }
    } else {
        EvidenceError::Io {
            path,
            source: error,
        }
    }
}
