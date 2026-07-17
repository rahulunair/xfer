use std::error::Error as StdError;
use std::fmt;
use std::io;

use crate::level_zero as ze;

pub type Result<T> = std::result::Result<T, BenchmarkError>;

#[derive(Debug)]
pub enum BenchmarkError {
    LevelZero(ze::LevelZeroError),
    LevelZeroOperation {
        phase: &'static str,
        error: ze::LevelZeroError,
    },
    NoDevices,
    InvalidFilter(String),
    SizeTooLarge(u64),
    VerificationFailed {
        case: String,
        offset: usize,
        expected: u8,
        actual: u8,
    },
    Statistics(String),
    Topology(String),
    Reporter(io::Error),
}

impl fmt::Display for BenchmarkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LevelZero(error) => write!(f, "{error}"),
            Self::LevelZeroOperation { phase, error } => write!(f, "{phase}: {error}"),
            Self::NoDevices => f.write_str("no Intel Level Zero GPU devices found"),
            Self::InvalidFilter(message) | Self::Statistics(message) | Self::Topology(message) => {
                f.write_str(message)
            }
            Self::Reporter(error) => write!(f, "benchmark reporter failed: {error}"),
            Self::SizeTooLarge(bytes) => write!(
                f,
                "requested allocation size {bytes} bytes does not fit in this process"
            ),
            Self::VerificationFailed {
                case,
                offset,
                expected,
                actual,
            } => write!(
                f,
                "{case} copied incorrect data at byte {offset}: expected 0x{expected:02x}, got 0x{actual:02x}"
            ),
        }
    }
}

impl StdError for BenchmarkError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::LevelZero(error) | Self::LevelZeroOperation { error, .. } => Some(error),
            Self::Reporter(error) => Some(error),
            Self::NoDevices
            | Self::InvalidFilter(_)
            | Self::SizeTooLarge(_)
            | Self::VerificationFailed { .. }
            | Self::Statistics(_)
            | Self::Topology(_) => None,
        }
    }
}

impl From<ze::LevelZeroError> for BenchmarkError {
    fn from(error: ze::LevelZeroError) -> Self {
        Self::LevelZero(error)
    }
}

#[derive(Debug)]
pub(crate) enum CaseExecutionError {
    Skip(String),
    Fatal(BenchmarkError),
}

pub(crate) fn ze_fatal<T>(
    result: ze::Result<T>,
    phase: &'static str,
) -> std::result::Result<T, CaseExecutionError> {
    result.map_err(|error| {
        CaseExecutionError::Fatal(BenchmarkError::LevelZeroOperation { phase, error })
    })
}

pub(crate) fn capability_skip_reason(error: &BenchmarkError) -> Option<String> {
    let BenchmarkError::LevelZeroOperation { phase, error } = error else {
        return None;
    };

    if error.is_capability_unavailable() && phase_allows_capability_skip(phase) {
        Some(format!("{phase}: {error}"))
    } else {
        None
    }
}

fn phase_allows_capability_skip(phase: &str) -> bool {
    matches!(
        phase,
        "create timestamp event pool"
            | "create timestamp event"
            | "reset timestamp event"
            | "record timestamped host-to-device copy"
            | "record timestamped device-to-host copy"
            | "record timestamped device-to-device copy"
            | "execute timestamped host-to-device copy"
            | "execute timestamped device-to-host copy"
            | "execute timestamped device-to-device copy"
            | "query timestamp event"
            | "record direct device-to-device copy"
            | "execute direct device-to-device copy"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const ZE_RESULT_ERROR_NOT_AVAILABLE: u32 = 0x7001_0001;

    fn unavailable(phase: &'static str) -> BenchmarkError {
        BenchmarkError::LevelZeroOperation {
            phase,
            error: ze::LevelZeroError::Ze {
                operation: "op",
                result: ZE_RESULT_ERROR_NOT_AVAILABLE,
            },
        }
    }

    #[test]
    fn capability_skip_is_phase_aware() {
        assert!(capability_skip_reason(&unavailable("create timestamp event pool")).is_some());
        assert!(capability_skip_reason(&unavailable("allocate device destination")).is_none());
        assert!(
            capability_skip_reason(&unavailable("synchronize timestamped host-to-device copy"))
                .is_none()
        );
        assert!(
            capability_skip_reason(&unavailable("synchronize direct device-to-device copy"))
                .is_none()
        );
    }
}
