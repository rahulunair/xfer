use std::fmt;
use std::time::Duration;

use crate::cli::{BenchMode, TimingMode, TransferClass};
use crate::stats::Summary;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextOptions {
    pub include_histogram: bool,
    pub summary_only: bool,
    pub color: ColorMode,
}

impl Default for TextOptions {
    fn default() -> Self {
        Self {
            include_histogram: true,
            summary_only: false,
            color: ColorMode::Never,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColorMode {
    Never,
    Ansi,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ListReport {
    pub devices: Vec<DeviceInfo>,
    pub peer_access: Vec<PeerAccessInfo>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostInfo {
    pub cpu_model: String,
    pub logical_cpus: usize,
    pub physical_cores: Option<usize>,
    pub sockets: Option<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SystemInfo {
    pub host: HostInfo,
    pub devices: Vec<DeviceInfo>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DeviceInfo {
    pub index: u32,
    pub name: String,
    pub pci_address: Option<String>,
    pub pcie_link: LinkInfo,
    pub queue_groups: Vec<QueueGroupInfo>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct QueueGroupInfo {
    pub ordinal: u32,
    pub flags: QueueFlags,
    pub queue_count: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QueueStreamInfo {
    pub group_ordinal: u32,
    pub queue_index: u32,
    pub flags: QueueFlags,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct QueueFlags {
    pub copy: bool,
    pub compute: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum LinkInfo {
    Known {
        generation: u8,
        width: u16,
        theoretical_gb_s: f64,
    },
    Unknown {
        reason: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PeerAccessInfo {
    pub from_device: u32,
    pub to_device: u32,
    pub access: PeerAccess,
    pub route: PeerRoute,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PeerAccess {
    Yes,
    No,
    Unknown(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PeerRoute {
    SameRootPort {
        root_port: String,
    },
    SharedUpstreamBridge {
        common_bridge: String,
    },
    DifferentRootPorts {
        host_bridge: String,
        source_root_port: String,
        destination_root_port: String,
    },
    CrossHostBridges {
        source_host_bridge: String,
        destination_host_bridge: String,
    },
    Unknown(String),
}

impl PeerAccess {
    pub fn as_field(&self) -> &str {
        match self {
            Self::Yes => "yes",
            Self::No => "no",
            Self::Unknown(_) => "unknown",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BenchReport {
    pub system: SystemInfo,
    pub cases: Vec<BenchCase>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BenchCase {
    pub mode: BenchMode,
    pub selected_group: Option<QueueGroupInfo>,
    pub streams: Vec<QueueStreamInfo>,
    pub second_phase_streams: Vec<QueueStreamInfo>,
    pub verification_stream: Option<QueueStreamInfo>,
    pub transfer_class: TransferClass,
    pub operation: Operation,
    pub source: Endpoint,
    pub destination: Endpoint,
    pub byte_count: u64,
    pub allocation: AllocationKind,
    pub timing: TimingMode,
    pub warmup: Duration,
    pub requested_samples: u32,
    pub pcie_link: LinkInfo,
    pub outcome: CaseOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Operation {
    HostToDevice,
    DeviceToHost,
    SameDevice,
    Direct {
        peer_access: PeerAccess,
        route: PeerRoute,
    },
    ExplicitStaged {
        route: PeerRoute,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Endpoint {
    Host,
    Device(u32),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AllocationKind {
    PinnedHost,
    Device,
    PinnedStaging,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CaseOutcome {
    Measured {
        time_summary: Box<Summary>,
        summary: Summary,
        samples_gb_s: Vec<f64>,
    },
    Skipped {
        reason: String,
    },
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Host => f.write_str("host"),
            Self::Device(index) => write!(f, "dev{index}"),
        }
    }
}

impl fmt::Display for AllocationKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PinnedHost => f.write_str("pinned-host"),
            Self::Device => f.write_str("device"),
            Self::PinnedStaging => f.write_str("pinned-staging"),
        }
    }
}
