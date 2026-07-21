//! Internal hardware-counter evidence types.
//!
//! This module deliberately records observations only. It does not decide
//! whether a transfer used a physical peer-to-peer path.

pub mod acs;
pub mod counters;
pub mod cpu;
pub mod error;
pub mod intel_perfmon;
pub mod linux_perf;
pub mod linux_pmu;

pub use acs::{
    BridgeEvidence as AcsBridgeEvidence, BridgeOutcome as AcsBridgeOutcome,
    BridgePathEvidence as AcsBridgePathEvidence, Capability as AcsCapability, Flags as AcsFlags,
    MalformedConfig as AcsMalformedConfig, ReadFailure as AcsReadFailure,
    parse_config as parse_acs_config, read_bridge_path as read_acs_bridge_path,
    read_bridge_path_from_sysfs as read_acs_bridge_path_from_sysfs,
};
pub use counters::{
    AttributableCounterValue, CounterDelta, CounterReading, CounterScaling, CounterSnapshot,
    CounterSource, EvidenceRunId, MultiplexingPolicy, SyntheticCounterSource, diff_snapshots,
};
pub use cpu::{CpuIdentity, CpuModel, CpuProfile, CpuVendor};
pub use error::{CounterFailure, CounterTimingContext, EvidenceError, PerfEventFailure, Result};
pub use linux_perf::{
    LinuxPerfCounterSet, LinuxPerfEventSpec, LinuxPerfGroupSpec, LinuxPerfMeasurement,
    parse_first_cpu,
};
pub use linux_pmu::{
    ConfigRegister, FormatField, FormatFieldName, PackedConfig, PmuAlias, PmuAliasName,
    PmuFieldValue, PmuFormat, PmuInstance, PmuInstanceId, PmuKind, PmuType, discover_pmus,
    discover_pmus_from_sysfs,
};
