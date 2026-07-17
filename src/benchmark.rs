#![allow(
    unsafe_code,
    clippy::cast_precision_loss,
    clippy::missing_errors_doc,
    clippy::module_name_repetitions,
    clippy::too_many_lines
)]

//! Benchmark orchestration for xefer.
//!
//! This module owns Level Zero discovery, benchmark case planning, command
//! submission, wall-clock sampling, and byte verification. Formatting stays in
//! `output`, statistics stay in `stats`, and raw FFI remains behind
//! `level_zero`.

use std::error::Error as StdError;
use std::fmt;
use std::time::{Duration, Instant};

use crate::cli::{BenchOptions, TimingMode, TransferClass};
use crate::level_zero as ze;
use crate::output::{
    AllocationKind, BenchCase, BenchReport, CaseOutcome, DeviceInfo, Endpoint, LinkInfo,
    ListReport, Operation, PeerAccess, PeerAccessInfo, QueueFlags, QueueGroupInfo,
};
use crate::pcie::{self, PcieLinkStatus, PcieLinkUnknown};
use crate::stats;

const INTEL_VENDOR_ID: u32 = 0x8086;
const ALLOCATION_ALIGNMENT: usize = 64;
const DEVICE_MEMORY_ORDINAL: u32 = 0;
const QUEUE_SYNC_TIMEOUT_NS: u64 = u64::MAX;
const PATTERN_SEED: u8 = 0x5a;
const SENTINEL_SEED: u8 = 0xa5;
const STAGED_DEVICE_TIMESTAMP_SKIP_REASON: &str = "device timestamp timing for explicit staged transfers is unsupported because the D2H and H2D legs can run on different device clock domains and cannot form one end-to-end device-time sample";

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

#[derive(Clone, Debug)]
struct DeviceRecord {
    index: u32,
    driver_index: usize,
    device: ze::Device,
    properties: ze::DeviceProperties,
    pci_address: Option<String>,
    pcie_link: LinkInfo,
    queues: Vec<QueueRecord>,
}

#[derive(Clone, Debug)]
struct QueueRecord {
    info: QueueGroupInfo,
}

#[derive(Debug)]
struct Topology {
    drivers: Vec<ze::Driver>,
    devices: Vec<DeviceRecord>,
    peer_access: Vec<PeerAccessInfo>,
}

#[derive(Clone, Debug)]
struct QueueSelection {
    info: QueueGroupInfo,
    skip_reason: Option<String>,
}

#[derive(Clone, Debug)]
struct CasePlan {
    transfer_class: TransferClass,
    operation: Operation,
    source: Endpoint,
    destination: Endpoint,
    allocation: AllocationKind,
    queue: QueueGroupInfo,
    pcie_link: LinkInfo,
    execution: ExecutionPlan,
    skip_reasons: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExecutionPlan {
    HostToDevice { device: usize },
    DeviceToHost { device: usize },
    SameDevice { device: usize },
    Direct { source: usize, destination: usize },
    Staged { source: usize, destination: usize },
}

#[derive(Debug)]
enum CaseExecutionError {
    Skip(String),
    Fatal(BenchmarkError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct VerificationMismatch {
    offset: usize,
    expected: u8,
    actual: u8,
}

pub fn list() -> Result<ListReport> {
    let topology = discover_topology()?;
    Ok(ListReport {
        devices: topology
            .devices
            .iter()
            .map(|device| DeviceInfo {
                index: device.index,
                name: device.properties.name.clone(),
                pci_address: device.pci_address.clone(),
                pcie_link: device.pcie_link.clone(),
                queue_groups: device
                    .queues
                    .iter()
                    .map(|queue| queue.info.clone())
                    .collect(),
            })
            .collect(),
        peer_access: topology.peer_access,
    })
}

pub fn bench(options: &BenchOptions) -> Result<BenchReport> {
    let topology = discover_topology()?;
    if topology.devices.is_empty() {
        return Err(BenchmarkError::NoDevices);
    }

    validate_filters(&topology, options)?;
    let byte_count = usize::try_from(options.size_bytes)
        .map_err(|_| BenchmarkError::SizeTooLarge(options.size_bytes))?;
    let plans = plan_cases(&topology, options);
    let mut cases = Vec::with_capacity(plans.len());

    for plan in plans {
        let outcome = if let Some(reason) = joined_skip_reasons(&plan.skip_reasons) {
            CaseOutcome::Skipped { reason }
        } else {
            match execute_case(&topology, &plan, options, byte_count) {
                Ok(outcome) => outcome,
                Err(CaseExecutionError::Skip(reason)) => CaseOutcome::Skipped { reason },
                Err(CaseExecutionError::Fatal(error)) => return Err(error),
            }
        };

        cases.push(plan.into_case(options, outcome));
    }

    Ok(BenchReport { cases })
}

fn discover_topology() -> Result<Topology> {
    let drivers = ze::initialize()?;
    let mut devices = Vec::new();

    for driver in &drivers {
        for device in driver.devices()? {
            let properties = device.properties()?;
            if properties.device_type != ze::DeviceType::Gpu
                || properties.vendor_id != INTEL_VENDOR_ID
            {
                continue;
            }

            let (pci_address, pcie_link) = read_device_link(&device);
            let queues = device
                .queue_groups()?
                .into_iter()
                .filter(|queue| {
                    queue.num_queues > 0 && (queue.supports_copy() || queue.supports_compute())
                })
                .map(|queue| QueueRecord {
                    info: QueueGroupInfo {
                        ordinal: queue.ordinal,
                        flags: QueueFlags {
                            copy: queue.supports_copy(),
                            compute: queue.supports_compute(),
                        },
                    },
                })
                .collect::<Vec<_>>();

            devices.push(DeviceRecord {
                index: u32::try_from(devices.len()).map_err(|_| {
                    BenchmarkError::Topology("too many devices to assign u32 indices".to_owned())
                })?,
                driver_index: device.driver_index(),
                device,
                properties,
                pci_address,
                pcie_link,
                queues,
            });
        }
    }

    let peer_access = query_peer_access(&devices);

    Ok(Topology {
        drivers,
        devices,
        peer_access,
    })
}

fn query_peer_access(devices: &[DeviceRecord]) -> Vec<PeerAccessInfo> {
    let mut peers = Vec::new();

    for from in devices {
        for to in devices {
            if from.index == to.index {
                continue;
            }

            let access = match from.device.can_access_peer(&to.device) {
                Ok(true) => PeerAccess::Yes,
                Ok(false) => PeerAccess::No,
                Err(error) => PeerAccess::Unknown(error.to_string()),
            };

            peers.push(PeerAccessInfo {
                from_device: from.index,
                to_device: to.index,
                access,
            });
        }
    }

    peers
}

fn read_device_link(device: &ze::Device) -> (Option<String>, LinkInfo) {
    let pci = match device.pci_address() {
        Ok(pci) => pci,
        Err(error) => {
            return (
                None,
                LinkInfo::Unknown {
                    reason: format!("zeDevicePciGetPropertiesExt unavailable: {error}"),
                },
            );
        }
    };

    let address = pcie::PciAddress::new(pci.domain, pci.bus, pci.device, pci.function);
    let address_text = address.map(|address| address.to_string());
    let link =
        match pcie::read_link_for_level_zero_pci(pci.domain, pci.bus, pci.device, pci.function) {
            PcieLinkStatus::Known(link) => LinkInfo::Known {
                generation: link.speed.generation.number(),
                width: link.width,
                theoretical_gb_s: link.theoretical_gb_s,
            },
            PcieLinkStatus::Unknown(reason) => LinkInfo::Unknown {
                reason: pcie_unknown_reason(&reason),
            },
        };

    (address_text, link)
}

fn pcie_unknown_reason(reason: &PcieLinkUnknown) -> String {
    match reason {
        PcieLinkUnknown::InvalidAddress => "invalid Level Zero PCI address".to_owned(),
        PcieLinkUnknown::MissingDevice(path) => {
            format!("missing sysfs PCI device {}", path.display())
        }
        PcieLinkUnknown::UnreliableDevicePath(path) => {
            format!("unreliable sysfs PCI device path {}", path.display())
        }
        PcieLinkUnknown::UnreadableField { path, error } => {
            format!("cannot read {}: {error}", path.display())
        }
        PcieLinkUnknown::UnrecognizedSpeed(speed) => {
            format!("unrecognized negotiated link speed '{speed}'")
        }
        PcieLinkUnknown::UnrecognizedWidth(width) => {
            format!("unrecognized negotiated link width '{width}'")
        }
    }
}

fn validate_filters(topology: &Topology, options: &BenchOptions) -> Result<()> {
    if let Some(index) = options.device {
        require_device(topology, index)?;
    }

    if let Some(index) = options.peer_device {
        require_device(topology, index)?;
    }

    if options.peer_device.is_some()
        && matches!(
            options.transfer_class,
            Some(TransferClass::H2D | TransferClass::D2H | TransferClass::D2DSameDevice)
        )
    {
        return Err(BenchmarkError::InvalidFilter(
            "--peer-device only applies to cross-device transfer classes".to_owned(),
        ));
    }

    if let (Some(device), Some(peer), Some(class)) =
        (options.device, options.peer_device, options.transfer_class)
    {
        if device == peer && matches!(class, TransferClass::D2DDirect | TransferClass::D2DStaged) {
            return Err(BenchmarkError::InvalidFilter(
                "cross-device transfer filters require --device and --peer-device to differ"
                    .to_owned(),
            ));
        }
    }

    Ok(())
}

fn require_device(topology: &Topology, index: u32) -> Result<()> {
    if topology.devices.iter().any(|device| device.index == index) {
        Ok(())
    } else {
        Err(BenchmarkError::InvalidFilter(format!(
            "device filter dev{index} did not match an Intel Level Zero GPU"
        )))
    }
}

fn plan_cases(topology: &Topology, options: &BenchOptions) -> Vec<CasePlan> {
    let mut plans = Vec::new();
    for class in selected_transfer_classes(options) {
        match class {
            TransferClass::H2D => {
                for device_index in selected_local_devices(topology, options) {
                    plans.extend(plan_h2d(topology, options, device_index));
                }
            }
            TransferClass::D2H => {
                for device_index in selected_local_devices(topology, options) {
                    plans.extend(plan_d2h(topology, options, device_index));
                }
            }
            TransferClass::D2DSameDevice => {
                for device_index in selected_local_devices(topology, options) {
                    plans.extend(plan_same_device(topology, options, device_index));
                }
            }
            TransferClass::D2DDirect => {
                plans.extend(plan_cross_device(
                    topology,
                    options,
                    TransferClass::D2DDirect,
                ));
            }
            TransferClass::D2DStaged => {
                plans.extend(plan_cross_device(
                    topology,
                    options,
                    TransferClass::D2DStaged,
                ));
            }
        }
    }

    plans
}

fn selected_transfer_classes(options: &BenchOptions) -> Vec<TransferClass> {
    options.transfer_class.map_or_else(
        || {
            vec![
                TransferClass::H2D,
                TransferClass::D2H,
                TransferClass::D2DSameDevice,
                TransferClass::D2DDirect,
                TransferClass::D2DStaged,
            ]
        },
        |class| vec![class],
    )
}

fn selected_local_devices(topology: &Topology, options: &BenchOptions) -> Vec<usize> {
    topology
        .devices
        .iter()
        .enumerate()
        .filter(|(_, device)| options.device.is_none_or(|index| index == device.index))
        .map(|(index, _)| index)
        .collect()
}

fn selected_peer_devices(
    topology: &Topology,
    options: &BenchOptions,
    source: &DeviceRecord,
) -> Vec<usize> {
    topology
        .devices
        .iter()
        .enumerate()
        .filter(|(_, peer)| peer.index != source.index)
        .filter(|(_, peer)| options.peer_device.is_none_or(|index| index == peer.index))
        .map(|(index, _)| index)
        .collect()
}

fn queue_selections(device: &DeviceRecord, options: &BenchOptions) -> Vec<QueueSelection> {
    if let Some(ordinal) = options.queue_ordinal {
        return match device
            .queues
            .iter()
            .find(|queue| queue.info.ordinal == ordinal)
        {
            Some(queue) => vec![QueueSelection {
                info: queue.info.clone(),
                skip_reason: None,
            }],
            None => vec![QueueSelection {
                info: QueueGroupInfo {
                    ordinal,
                    flags: QueueFlags::default(),
                },
                skip_reason: Some(format!(
                    "dev{} has no usable queue group ordinal {ordinal}",
                    device.index
                )),
            }],
        };
    }

    if device.queues.is_empty() {
        vec![QueueSelection {
            info: QueueGroupInfo {
                ordinal: 0,
                flags: QueueFlags::default(),
            },
            skip_reason: Some(format!("dev{} has no usable queue groups", device.index)),
        }]
    } else {
        device
            .queues
            .iter()
            .map(|queue| QueueSelection {
                info: queue.info.clone(),
                skip_reason: None,
            })
            .collect()
    }
}

fn plan_h2d(topology: &Topology, options: &BenchOptions, device_index: usize) -> Vec<CasePlan> {
    let device = &topology.devices[device_index];
    queue_selections(device, options)
        .into_iter()
        .map(|queue| {
            let mut reasons = common_skip_reasons(&queue);
            check_device_allocation_size(options.size_bytes, &[device], &mut reasons);
            CasePlan {
                transfer_class: TransferClass::H2D,
                operation: Operation::HostToDevice,
                source: Endpoint::Host,
                destination: Endpoint::Device(device.index),
                allocation: AllocationKind::PinnedHost,
                queue: queue.info,
                pcie_link: device.pcie_link.clone(),
                execution: ExecutionPlan::HostToDevice {
                    device: device_index,
                },
                skip_reasons: reasons,
            }
        })
        .collect()
}

fn plan_d2h(topology: &Topology, options: &BenchOptions, device_index: usize) -> Vec<CasePlan> {
    let device = &topology.devices[device_index];
    queue_selections(device, options)
        .into_iter()
        .map(|queue| {
            let mut reasons = common_skip_reasons(&queue);
            check_device_allocation_size(options.size_bytes, &[device], &mut reasons);
            CasePlan {
                transfer_class: TransferClass::D2H,
                operation: Operation::DeviceToHost,
                source: Endpoint::Device(device.index),
                destination: Endpoint::Host,
                allocation: AllocationKind::PinnedHost,
                queue: queue.info,
                pcie_link: device.pcie_link.clone(),
                execution: ExecutionPlan::DeviceToHost {
                    device: device_index,
                },
                skip_reasons: reasons,
            }
        })
        .collect()
}

fn plan_same_device(
    topology: &Topology,
    options: &BenchOptions,
    device_index: usize,
) -> Vec<CasePlan> {
    let device = &topology.devices[device_index];
    queue_selections(device, options)
        .into_iter()
        .map(|queue| {
            let mut reasons = common_skip_reasons(&queue);
            check_device_allocation_size(options.size_bytes, &[device], &mut reasons);
            CasePlan {
                transfer_class: TransferClass::D2DSameDevice,
                operation: Operation::SameDevice,
                source: Endpoint::Device(device.index),
                destination: Endpoint::Device(device.index),
                allocation: AllocationKind::Device,
                queue: queue.info,
                pcie_link: LinkInfo::Unknown {
                    reason: "same-device transfer has no single negotiated PCIe link".to_owned(),
                },
                execution: ExecutionPlan::SameDevice {
                    device: device_index,
                },
                skip_reasons: reasons,
            }
        })
        .collect()
}

fn plan_cross_device(
    topology: &Topology,
    options: &BenchOptions,
    class: TransferClass,
) -> Vec<CasePlan> {
    let mut plans = Vec::new();

    for source_index in selected_local_devices(topology, options) {
        let source = &topology.devices[source_index];
        for destination_index in selected_peer_devices(topology, options, source) {
            let destination = &topology.devices[destination_index];

            for queue in queue_selections(source, options) {
                let mut reasons = common_skip_reasons(&queue);
                check_device_allocation_size(
                    options.size_bytes,
                    &[source, destination],
                    &mut reasons,
                );
                require_destination_copy_queue(destination, queue.info.ordinal, &mut reasons);

                let (operation, allocation, execution) = match class {
                    TransferClass::D2DDirect => {
                        if source.driver_index != destination.driver_index {
                            reasons.push(
                                "direct cross-device copy requires both devices in one Level Zero driver/context; current level_zero.rs exposes context creation per driver"
                                    .to_owned(),
                            );
                        }
                        (
                            Operation::Direct {
                                peer_access: topology
                                    .peer_access_between(source.index, destination.index),
                            },
                            AllocationKind::Device,
                            ExecutionPlan::Direct {
                                source: source_index,
                                destination: destination_index,
                            },
                        )
                    }
                    TransferClass::D2DStaged => {
                        if source.driver_index != destination.driver_index {
                            reasons.push(
                                "explicit staged transfers across Level Zero drivers require one pinned host buffer usable by both contexts; current level_zero.rs exposes only context-owned host allocations"
                                    .to_owned(),
                            );
                        }
                        if options.timing == TimingMode::DeviceTimestamps {
                            reasons.push(STAGED_DEVICE_TIMESTAMP_SKIP_REASON.to_owned());
                        }
                        (
                            Operation::ExplicitStaged,
                            AllocationKind::PinnedStaging,
                            ExecutionPlan::Staged {
                                source: source_index,
                                destination: destination_index,
                            },
                        )
                    }
                    TransferClass::H2D | TransferClass::D2H | TransferClass::D2DSameDevice => {
                        unreachable!("non-cross transfer class routed to cross planner")
                    }
                };

                plans.push(CasePlan {
                    transfer_class: class,
                    operation,
                    source: Endpoint::Device(source.index),
                    destination: Endpoint::Device(destination.index),
                    allocation,
                    queue: queue.info,
                    pcie_link: LinkInfo::Unknown {
                        reason: "cross-device transfer has no single negotiated PCIe link"
                            .to_owned(),
                    },
                    execution,
                    skip_reasons: reasons,
                });
            }
        }
    }

    plans
}

fn common_skip_reasons(queue: &QueueSelection) -> Vec<String> {
    let mut reasons = Vec::new();
    if let Some(reason) = &queue.skip_reason {
        reasons.push(reason.clone());
    } else if !queue.info.flags.copy {
        reasons.push(format!(
            "queue group ordinal {} does not advertise copy capability",
            queue.info.ordinal
        ));
    }

    reasons
}

fn check_device_allocation_size(bytes: u64, devices: &[&DeviceRecord], reasons: &mut Vec<String>) {
    for device in devices {
        if bytes > device.properties.max_mem_alloc_size {
            reasons.push(format!(
                "requested {bytes} bytes exceeds dev{} max allocation size {} bytes",
                device.index, device.properties.max_mem_alloc_size
            ));
        }
    }
}

fn require_destination_copy_queue(
    destination: &DeviceRecord,
    ordinal: u32,
    reasons: &mut Vec<String>,
) {
    match destination
        .queues
        .iter()
        .find(|queue| queue.info.ordinal == ordinal)
    {
        Some(queue) if queue.info.flags.copy => {}
        Some(_) => reasons.push(format!(
            "destination dev{} queue group ordinal {ordinal} does not advertise copy capability",
            destination.index
        )),
        None => reasons.push(format!(
            "destination dev{} has no usable queue group ordinal {ordinal}",
            destination.index
        )),
    }
}

fn joined_skip_reasons(reasons: &[String]) -> Option<String> {
    if reasons.is_empty() {
        None
    } else {
        Some(reasons.join("; "))
    }
}

impl Topology {
    fn driver_for(&self, device: &DeviceRecord) -> Result<&ze::Driver> {
        self.drivers
            .iter()
            .find(|driver| driver.index() == device.driver_index)
            .ok_or_else(|| {
                BenchmarkError::Topology(format!(
                    "driver {} for dev{} disappeared from topology",
                    device.driver_index, device.index
                ))
            })
    }

    fn peer_access_between(&self, from_device: u32, to_device: u32) -> PeerAccess {
        self.peer_access
            .iter()
            .find(|peer| peer.from_device == from_device && peer.to_device == to_device)
            .map_or_else(
                || PeerAccess::Unknown("zeDeviceCanAccessPeer was not queried".to_owned()),
                |peer| peer.access.clone(),
            )
    }
}

impl CasePlan {
    fn into_case(self, options: &BenchOptions, outcome: CaseOutcome) -> BenchCase {
        BenchCase {
            transfer_class: self.transfer_class,
            operation: self.operation,
            source: self.source,
            destination: self.destination,
            byte_count: options.size_bytes,
            allocation: self.allocation,
            queue: self.queue,
            timing: options.timing,
            warmup: options.warmup,
            requested_samples: options.samples,
            pcie_link: self.pcie_link,
            outcome,
        }
    }

    fn label(&self) -> String {
        format!(
            "{} {} -> {} queue {}",
            self.transfer_class, self.source, self.destination, self.queue.ordinal
        )
    }
}

fn execute_case(
    topology: &Topology,
    plan: &CasePlan,
    options: &BenchOptions,
    bytes: usize,
) -> std::result::Result<CaseOutcome, CaseExecutionError> {
    let durations = match plan.execution {
        ExecutionPlan::HostToDevice { device } => {
            measure_h2d(topology, plan, options, bytes, device)?
        }
        ExecutionPlan::DeviceToHost { device } => {
            measure_d2h(topology, plan, options, bytes, device)?
        }
        ExecutionPlan::SameDevice { device } => {
            measure_same_device(topology, plan, options, bytes, device)?
        }
        ExecutionPlan::Direct {
            source,
            destination,
        } => measure_direct(topology, plan, options, bytes, source, destination)?,
        ExecutionPlan::Staged {
            source,
            destination,
        } => measure_staged(topology, plan, options, bytes, source, destination)?,
    };

    let samples_gb_s = durations
        .iter()
        .map(|duration| {
            duration_to_gb_s(options.size_bytes, *duration).ok_or_else(|| {
                CaseExecutionError::Fatal(BenchmarkError::Statistics(format!(
                    "{} produced a zero-duration timing sample",
                    plan.label()
                )))
            })
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let summary = stats::summarize(&samples_gb_s).ok_or_else(|| {
        CaseExecutionError::Fatal(BenchmarkError::Statistics(format!(
            "{} produced invalid sample statistics",
            plan.label()
        )))
    })?;

    Ok(CaseOutcome::Measured {
        summary,
        samples_gb_s,
    })
}

fn measure_h2d(
    topology: &Topology,
    plan: &CasePlan,
    options: &BenchOptions,
    bytes: usize,
    device_index: usize,
) -> std::result::Result<Vec<Duration>, CaseExecutionError> {
    let device = &topology.devices[device_index];
    let driver = topology
        .driver_for(device)
        .map_err(CaseExecutionError::Fatal)?;
    let context = ze_fatal(driver.create_context(), "create context")?;
    let queue = ze_fatal(
        context.create_command_queue(&device.device, plan.queue.ordinal),
        "create command queue",
    )?;
    let list = ze_fatal(
        context.create_command_list(&device.device, plan.queue.ordinal),
        "create command list",
    )?;
    let mut source = ze_fatal(
        context.alloc_host(bytes, ALLOCATION_ALIGNMENT),
        "allocate pinned host source",
    )?;
    let mut verify = ze_fatal(
        context.alloc_host(bytes, ALLOCATION_ALIGNMENT),
        "allocate pinned host verification buffer",
    )?;
    let device_dst = ze_fatal(
        context.alloc_device(
            &device.device,
            bytes,
            ALLOCATION_ALIGNMENT,
            DEVICE_MEMORY_ORDINAL,
        ),
        "allocate device destination",
    )?;
    let timestamp_pool = create_timestamp_pool(&context, options.timing, &[&device.device])?;
    let timestamp_event = create_timestamp_event(timestamp_pool.as_ref())?;

    fill_pattern(source.as_mut_slice(), PATTERN_SEED);
    warmup(options.warmup, || {
        sample_h2d(
            options.timing,
            &queue,
            &list,
            &device_dst,
            &source,
            bytes,
            timestamp_event.as_ref(),
            &device.properties,
            plan,
        )
        .map(|_| ())
    })?;

    let sample_count = usize::try_from(options.samples).map_err(|_| {
        CaseExecutionError::Fatal(BenchmarkError::Statistics(
            "sample count does not fit usize".to_owned(),
        ))
    })?;
    let mut durations = Vec::with_capacity(sample_count);
    for _ in 0..options.samples {
        fill_pattern(verify.as_mut_slice(), SENTINEL_SEED);
        ze_fatal(
            copy_h2d_sync(&queue, &list, &device_dst, &verify, bytes),
            "clear device destination before sample",
        )?;

        let elapsed = sample_h2d(
            options.timing,
            &queue,
            &list,
            &device_dst,
            &source,
            bytes,
            timestamp_event.as_ref(),
            &device.properties,
            plan,
        )?;

        ze_fatal(
            copy_d2h_sync(&queue, &list, &mut verify, &device_dst, bytes),
            "copy device destination for verification",
        )?;
        verify_or_fail(verify.as_slice(), PATTERN_SEED, plan)?;
        durations.push(elapsed);
    }

    Ok(durations)
}

fn measure_d2h(
    topology: &Topology,
    plan: &CasePlan,
    options: &BenchOptions,
    bytes: usize,
    device_index: usize,
) -> std::result::Result<Vec<Duration>, CaseExecutionError> {
    let device = &topology.devices[device_index];
    let driver = topology
        .driver_for(device)
        .map_err(CaseExecutionError::Fatal)?;
    let context = ze_fatal(driver.create_context(), "create context")?;
    let queue = ze_fatal(
        context.create_command_queue(&device.device, plan.queue.ordinal),
        "create command queue",
    )?;
    let list = ze_fatal(
        context.create_command_list(&device.device, plan.queue.ordinal),
        "create command list",
    )?;
    let mut source = ze_fatal(
        context.alloc_host(bytes, ALLOCATION_ALIGNMENT),
        "allocate pinned host source",
    )?;
    let mut destination = ze_fatal(
        context.alloc_host(bytes, ALLOCATION_ALIGNMENT),
        "allocate pinned host destination",
    )?;
    let device_src = ze_fatal(
        context.alloc_device(
            &device.device,
            bytes,
            ALLOCATION_ALIGNMENT,
            DEVICE_MEMORY_ORDINAL,
        ),
        "allocate device source",
    )?;
    let timestamp_pool = create_timestamp_pool(&context, options.timing, &[&device.device])?;
    let timestamp_event = create_timestamp_event(timestamp_pool.as_ref())?;

    fill_pattern(source.as_mut_slice(), PATTERN_SEED);
    ze_fatal(
        copy_h2d_sync(&queue, &list, &device_src, &source, bytes),
        "initialize device source",
    )?;
    warmup(options.warmup, || {
        sample_d2h(
            options.timing,
            &queue,
            &list,
            &mut destination,
            &device_src,
            bytes,
            timestamp_event.as_ref(),
            &device.properties,
            plan,
        )
        .map(|_| ())
    })?;

    let sample_count = usize::try_from(options.samples).map_err(|_| {
        CaseExecutionError::Fatal(BenchmarkError::Statistics(
            "sample count does not fit usize".to_owned(),
        ))
    })?;
    let mut durations = Vec::with_capacity(sample_count);
    for _ in 0..options.samples {
        fill_pattern(destination.as_mut_slice(), SENTINEL_SEED);
        let elapsed = sample_d2h(
            options.timing,
            &queue,
            &list,
            &mut destination,
            &device_src,
            bytes,
            timestamp_event.as_ref(),
            &device.properties,
            plan,
        )?;
        verify_or_fail(destination.as_slice(), PATTERN_SEED, plan)?;
        durations.push(elapsed);
    }

    Ok(durations)
}

fn measure_same_device(
    topology: &Topology,
    plan: &CasePlan,
    options: &BenchOptions,
    bytes: usize,
    device_index: usize,
) -> std::result::Result<Vec<Duration>, CaseExecutionError> {
    let device = &topology.devices[device_index];
    let driver = topology
        .driver_for(device)
        .map_err(CaseExecutionError::Fatal)?;
    let context = ze_fatal(driver.create_context(), "create context")?;
    let queue = ze_fatal(
        context.create_command_queue(&device.device, plan.queue.ordinal),
        "create command queue",
    )?;
    let list = ze_fatal(
        context.create_command_list(&device.device, plan.queue.ordinal),
        "create command list",
    )?;
    let mut host = ze_fatal(
        context.alloc_host(bytes, ALLOCATION_ALIGNMENT),
        "allocate pinned host source/verification buffer",
    )?;
    let device_src = ze_fatal(
        context.alloc_device(
            &device.device,
            bytes,
            ALLOCATION_ALIGNMENT,
            DEVICE_MEMORY_ORDINAL,
        ),
        "allocate device source",
    )?;
    let device_dst = ze_fatal(
        context.alloc_device(
            &device.device,
            bytes,
            ALLOCATION_ALIGNMENT,
            DEVICE_MEMORY_ORDINAL,
        ),
        "allocate device destination",
    )?;
    let timestamp_pool = create_timestamp_pool(&context, options.timing, &[&device.device])?;
    let timestamp_event = create_timestamp_event(timestamp_pool.as_ref())?;

    fill_pattern(host.as_mut_slice(), PATTERN_SEED);
    ze_fatal(
        copy_h2d_sync(&queue, &list, &device_src, &host, bytes),
        "initialize device source",
    )?;
    warmup(options.warmup, || {
        sample_d2d(
            options.timing,
            &queue,
            &list,
            &device_dst,
            &device_src,
            bytes,
            timestamp_event.as_ref(),
            &device.properties,
            plan,
        )
        .map(|_| ())
    })?;

    let sample_count = usize::try_from(options.samples).map_err(|_| {
        CaseExecutionError::Fatal(BenchmarkError::Statistics(
            "sample count does not fit usize".to_owned(),
        ))
    })?;
    let mut durations = Vec::with_capacity(sample_count);
    for _ in 0..options.samples {
        fill_pattern(host.as_mut_slice(), SENTINEL_SEED);
        ze_fatal(
            copy_h2d_sync(&queue, &list, &device_dst, &host, bytes),
            "clear device destination before sample",
        )?;
        let elapsed = sample_d2d(
            options.timing,
            &queue,
            &list,
            &device_dst,
            &device_src,
            bytes,
            timestamp_event.as_ref(),
            &device.properties,
            plan,
        )?;
        ze_fatal(
            copy_d2h_sync(&queue, &list, &mut host, &device_dst, bytes),
            "copy device destination for verification",
        )?;
        verify_or_fail(host.as_slice(), PATTERN_SEED, plan)?;
        durations.push(elapsed);
    }

    Ok(durations)
}

fn measure_direct(
    topology: &Topology,
    plan: &CasePlan,
    options: &BenchOptions,
    bytes: usize,
    source_index: usize,
    destination_index: usize,
) -> std::result::Result<Vec<Duration>, CaseExecutionError> {
    let source = &topology.devices[source_index];
    let destination = &topology.devices[destination_index];
    let driver = topology
        .driver_for(source)
        .map_err(CaseExecutionError::Fatal)?;
    let context = ze_fatal(driver.create_context(), "create context")?;
    let source_queue = ze_fatal(
        context.create_command_queue(&source.device, plan.queue.ordinal),
        "create source command queue",
    )?;
    let source_list = ze_fatal(
        context.create_command_list(&source.device, plan.queue.ordinal),
        "create source command list",
    )?;
    let destination_queue = ze_fatal(
        context.create_command_queue(&destination.device, plan.queue.ordinal),
        "create destination command queue",
    )?;
    let destination_list = ze_fatal(
        context.create_command_list(&destination.device, plan.queue.ordinal),
        "create destination command list",
    )?;
    let mut host = ze_fatal(
        context.alloc_host(bytes, ALLOCATION_ALIGNMENT),
        "allocate pinned host source/verification buffer",
    )?;
    let device_src = ze_fatal(
        context.alloc_device(
            &source.device,
            bytes,
            ALLOCATION_ALIGNMENT,
            DEVICE_MEMORY_ORDINAL,
        ),
        "allocate source device buffer",
    )?;
    let device_dst = ze_fatal(
        context.alloc_device(
            &destination.device,
            bytes,
            ALLOCATION_ALIGNMENT,
            DEVICE_MEMORY_ORDINAL,
        ),
        "allocate destination device buffer",
    )?;
    let timestamp_pool = create_timestamp_pool(&context, options.timing, &[&source.device])?;
    let timestamp_event = create_timestamp_event(timestamp_pool.as_ref())?;

    fill_pattern(host.as_mut_slice(), PATTERN_SEED);
    ze_fatal(
        copy_h2d_sync(&source_queue, &source_list, &device_src, &host, bytes),
        "initialize source device buffer",
    )?;
    warmup(options.warmup, || {
        sample_d2d(
            options.timing,
            &source_queue,
            &source_list,
            &device_dst,
            &device_src,
            bytes,
            timestamp_event.as_ref(),
            &source.properties,
            plan,
        )
        .map(|_| ())
    })?;

    let sample_count = usize::try_from(options.samples).map_err(|_| {
        CaseExecutionError::Fatal(BenchmarkError::Statistics(
            "sample count does not fit usize".to_owned(),
        ))
    })?;
    let mut durations = Vec::with_capacity(sample_count);
    for _ in 0..options.samples {
        fill_pattern(host.as_mut_slice(), SENTINEL_SEED);
        ze_fatal(
            copy_h2d_sync(
                &destination_queue,
                &destination_list,
                &device_dst,
                &host,
                bytes,
            ),
            "clear destination device buffer before sample",
        )?;
        let elapsed = sample_d2d(
            options.timing,
            &source_queue,
            &source_list,
            &device_dst,
            &device_src,
            bytes,
            timestamp_event.as_ref(),
            &source.properties,
            plan,
        )?;
        ze_fatal(
            copy_d2h_sync(
                &destination_queue,
                &destination_list,
                &mut host,
                &device_dst,
                bytes,
            ),
            "copy destination device buffer for verification",
        )?;
        verify_or_fail(host.as_slice(), PATTERN_SEED, plan)?;
        durations.push(elapsed);
    }

    Ok(durations)
}

fn measure_staged(
    topology: &Topology,
    plan: &CasePlan,
    options: &BenchOptions,
    bytes: usize,
    source_index: usize,
    destination_index: usize,
) -> std::result::Result<Vec<Duration>, CaseExecutionError> {
    if options.timing == TimingMode::DeviceTimestamps {
        return Err(CaseExecutionError::Skip(
            STAGED_DEVICE_TIMESTAMP_SKIP_REASON.to_owned(),
        ));
    }

    let source = &topology.devices[source_index];
    let destination = &topology.devices[destination_index];
    let driver = topology
        .driver_for(source)
        .map_err(CaseExecutionError::Fatal)?;
    let context = ze_fatal(driver.create_context(), "create context")?;
    let source_queue = ze_fatal(
        context.create_command_queue(&source.device, plan.queue.ordinal),
        "create source command queue",
    )?;
    let source_list = ze_fatal(
        context.create_command_list(&source.device, plan.queue.ordinal),
        "create source command list",
    )?;
    let destination_queue = ze_fatal(
        context.create_command_queue(&destination.device, plan.queue.ordinal),
        "create destination command queue",
    )?;
    let destination_list = ze_fatal(
        context.create_command_list(&destination.device, plan.queue.ordinal),
        "create destination command list",
    )?;
    let mut host_source = ze_fatal(
        context.alloc_host(bytes, ALLOCATION_ALIGNMENT),
        "allocate pinned host source",
    )?;
    let mut staging = ze_fatal(
        context.alloc_host(bytes, ALLOCATION_ALIGNMENT),
        "allocate pinned host staging buffer",
    )?;
    let mut verify = ze_fatal(
        context.alloc_host(bytes, ALLOCATION_ALIGNMENT),
        "allocate pinned host verification buffer",
    )?;
    let device_src = ze_fatal(
        context.alloc_device(
            &source.device,
            bytes,
            ALLOCATION_ALIGNMENT,
            DEVICE_MEMORY_ORDINAL,
        ),
        "allocate source device buffer",
    )?;
    let device_dst = ze_fatal(
        context.alloc_device(
            &destination.device,
            bytes,
            ALLOCATION_ALIGNMENT,
            DEVICE_MEMORY_ORDINAL,
        ),
        "allocate destination device buffer",
    )?;

    fill_pattern(host_source.as_mut_slice(), PATTERN_SEED);
    ze_fatal(
        copy_h2d_sync(
            &source_queue,
            &source_list,
            &device_src,
            &host_source,
            bytes,
        ),
        "initialize source device buffer",
    )?;
    warmup(options.warmup, || {
        ze_fatal(
            copy_staged_sync(
                &source_queue,
                &source_list,
                &destination_queue,
                &destination_list,
                &mut staging,
                &device_src,
                &device_dst,
                bytes,
            ),
            "warm up explicit staged copy",
        )
    })?;

    let sample_count = usize::try_from(options.samples).map_err(|_| {
        CaseExecutionError::Fatal(BenchmarkError::Statistics(
            "sample count does not fit usize".to_owned(),
        ))
    })?;
    let mut durations = Vec::with_capacity(sample_count);
    for _ in 0..options.samples {
        fill_pattern(verify.as_mut_slice(), SENTINEL_SEED);
        ze_fatal(
            copy_h2d_sync(
                &destination_queue,
                &destination_list,
                &device_dst,
                &verify,
                bytes,
            ),
            "clear destination device buffer before sample",
        )?;
        let elapsed = ze_fatal(
            time_staged_sync(
                &source_queue,
                &source_list,
                &destination_queue,
                &destination_list,
                &mut staging,
                &device_src,
                &device_dst,
                bytes,
            ),
            "sample explicit staged copy",
        )?;
        ze_fatal(
            copy_d2h_sync(
                &destination_queue,
                &destination_list,
                &mut verify,
                &device_dst,
                bytes,
            ),
            "copy destination device buffer for verification",
        )?;
        verify_or_fail(verify.as_slice(), PATTERN_SEED, plan)?;
        durations.push(elapsed);
    }

    Ok(durations)
}

fn warmup<F>(duration: Duration, mut run_once: F) -> std::result::Result<(), CaseExecutionError>
where
    F: FnMut() -> std::result::Result<(), CaseExecutionError>,
{
    if duration.is_zero() {
        return Ok(());
    }

    let started = Instant::now();
    while started.elapsed() < duration {
        run_once()?;
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn sample_h2d(
    timing: TimingMode,
    queue: &ze::CommandQueue<'_>,
    list: &ze::CommandList<'_>,
    dst: &ze::DeviceAllocation<'_>,
    src: &ze::HostAllocation<'_>,
    bytes: usize,
    timestamp_event: Option<&ze::Event<'_>>,
    properties: &ze::DeviceProperties,
    plan: &CasePlan,
) -> std::result::Result<Duration, CaseExecutionError> {
    match timing {
        TimingMode::WallClock => ze_fatal(
            time_h2d_sync(queue, list, dst, src, bytes),
            "sample host-to-device copy",
        ),
        TimingMode::DeviceTimestamps => {
            let event = require_timestamp_event(timestamp_event, plan)?;
            event
                .host_reset()
                .map_err(|error| timestamp_level_zero_error("reset timestamp event", error))?;
            prepare_h2d_list(list, dst, src, bytes, Some(event)).map_err(|error| {
                timestamp_level_zero_error("record timestamped host-to-device copy", error)
            })?;
            queue.execute(&[list]).map_err(|error| {
                timestamp_level_zero_error("execute timestamped host-to-device copy", error)
            })?;
            queue.synchronize(QUEUE_SYNC_TIMEOUT_NS).map_err(|error| {
                timestamp_level_zero_error("synchronize timestamped host-to-device copy", error)
            })?;
            timestamp_duration(event, properties, plan)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn sample_d2h(
    timing: TimingMode,
    queue: &ze::CommandQueue<'_>,
    list: &ze::CommandList<'_>,
    dst: &mut ze::HostAllocation<'_>,
    src: &ze::DeviceAllocation<'_>,
    bytes: usize,
    timestamp_event: Option<&ze::Event<'_>>,
    properties: &ze::DeviceProperties,
    plan: &CasePlan,
) -> std::result::Result<Duration, CaseExecutionError> {
    match timing {
        TimingMode::WallClock => ze_fatal(
            time_d2h_sync(queue, list, dst, src, bytes),
            "sample device-to-host copy",
        ),
        TimingMode::DeviceTimestamps => {
            let event = require_timestamp_event(timestamp_event, plan)?;
            event
                .host_reset()
                .map_err(|error| timestamp_level_zero_error("reset timestamp event", error))?;
            prepare_d2h_list(list, dst, src, bytes, Some(event)).map_err(|error| {
                timestamp_level_zero_error("record timestamped device-to-host copy", error)
            })?;
            queue.execute(&[list]).map_err(|error| {
                timestamp_level_zero_error("execute timestamped device-to-host copy", error)
            })?;
            queue.synchronize(QUEUE_SYNC_TIMEOUT_NS).map_err(|error| {
                timestamp_level_zero_error("synchronize timestamped device-to-host copy", error)
            })?;
            timestamp_duration(event, properties, plan)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn sample_d2d(
    timing: TimingMode,
    queue: &ze::CommandQueue<'_>,
    list: &ze::CommandList<'_>,
    dst: &ze::DeviceAllocation<'_>,
    src: &ze::DeviceAllocation<'_>,
    bytes: usize,
    timestamp_event: Option<&ze::Event<'_>>,
    properties: &ze::DeviceProperties,
    plan: &CasePlan,
) -> std::result::Result<Duration, CaseExecutionError> {
    match timing {
        TimingMode::WallClock => ze_fatal(
            time_d2d_sync(queue, list, dst, src, bytes),
            "sample device-to-device copy",
        ),
        TimingMode::DeviceTimestamps => {
            let event = require_timestamp_event(timestamp_event, plan)?;
            event
                .host_reset()
                .map_err(|error| timestamp_level_zero_error("reset timestamp event", error))?;
            prepare_d2d_list(list, dst, src, bytes, Some(event)).map_err(|error| {
                timestamp_level_zero_error("record timestamped device-to-device copy", error)
            })?;
            queue.execute(&[list]).map_err(|error| {
                timestamp_level_zero_error("execute timestamped device-to-device copy", error)
            })?;
            queue.synchronize(QUEUE_SYNC_TIMEOUT_NS).map_err(|error| {
                timestamp_level_zero_error("synchronize timestamped device-to-device copy", error)
            })?;
            timestamp_duration(event, properties, plan)
        }
    }
}

fn create_timestamp_pool<'context>(
    context: &'context ze::Context<'_>,
    timing: TimingMode,
    devices: &[&ze::Device],
) -> std::result::Result<Option<ze::EventPool<'context>>, CaseExecutionError> {
    if timing == TimingMode::DeviceTimestamps {
        ze_fatal(
            ze::EventPool::kernel_timestamps(context, devices, 1),
            "create timestamp event pool",
        )
        .map(Some)
    } else {
        Ok(None)
    }
}

fn create_timestamp_event<'pool>(
    pool: Option<&'pool ze::EventPool<'_>>,
) -> std::result::Result<Option<ze::Event<'pool>>, CaseExecutionError> {
    pool.map(|pool| ze_fatal(pool.create_event(0), "create timestamp event"))
        .transpose()
}

fn require_timestamp_event<'event>(
    event: Option<&'event ze::Event<'_>>,
    plan: &CasePlan,
) -> std::result::Result<&'event ze::Event<'event>, CaseExecutionError> {
    event.ok_or_else(|| {
        CaseExecutionError::Fatal(BenchmarkError::Topology(format!(
            "{} missing timestamp event for device timestamp sample",
            plan.label()
        )))
    })
}

fn timestamp_duration(
    event: &ze::Event<'_>,
    properties: &ze::DeviceProperties,
    plan: &CasePlan,
) -> std::result::Result<Duration, CaseExecutionError> {
    let timestamp = ze_fatal(event.query_kernel_timestamp(), "query timestamp event")?;
    let ticks = elapsed_timestamp_ticks(
        timestamp.global_kernel_start,
        timestamp.global_kernel_end,
        properties.kernel_timestamp_valid_bits,
    );
    let nanos = u128::from(ticks) * u128::from(properties.timer_resolution);
    let nanos = u64::try_from(nanos).map_err(|_| {
        CaseExecutionError::Fatal(BenchmarkError::Statistics(format!(
            "{} device timestamp duration overflowed u64 nanoseconds",
            plan.label()
        )))
    })?;
    Ok(Duration::from_nanos(nanos))
}

fn elapsed_timestamp_ticks(start: u64, end: u64, valid_bits: u32) -> u64 {
    let mask = timestamp_mask(valid_bits);
    (end.wrapping_sub(start)) & mask
}

fn timestamp_mask(valid_bits: u32) -> u64 {
    match valid_bits {
        0 | 64.. => u64::MAX,
        bits => (1_u64 << bits) - 1,
    }
}

fn timestamp_level_zero_error(
    phase: &'static str,
    error: ze::LevelZeroError,
) -> CaseExecutionError {
    CaseExecutionError::Fatal(BenchmarkError::LevelZeroOperation { phase, error })
}

fn copy_h2d_sync(
    queue: &ze::CommandQueue<'_>,
    list: &ze::CommandList<'_>,
    dst: &ze::DeviceAllocation<'_>,
    src: &ze::HostAllocation<'_>,
    bytes: usize,
) -> ze::Result<()> {
    prepare_h2d_list(list, dst, src, bytes, None)?;
    queue.execute(&[list])?;
    queue.synchronize(QUEUE_SYNC_TIMEOUT_NS)
}

fn time_h2d_sync(
    queue: &ze::CommandQueue<'_>,
    list: &ze::CommandList<'_>,
    dst: &ze::DeviceAllocation<'_>,
    src: &ze::HostAllocation<'_>,
    bytes: usize,
) -> ze::Result<Duration> {
    prepare_h2d_list(list, dst, src, bytes, None)?;
    let started = Instant::now();
    queue.execute(&[list])?;
    queue.synchronize(QUEUE_SYNC_TIMEOUT_NS)?;
    Ok(started.elapsed())
}

fn prepare_h2d_list(
    list: &ze::CommandList<'_>,
    dst: &ze::DeviceAllocation<'_>,
    src: &ze::HostAllocation<'_>,
    bytes: usize,
    signal: Option<&ze::Event<'_>>,
) -> ze::Result<()> {
    list.reset()?;
    unsafe {
        // SAFETY: source and destination allocations outlive queue execution, byte count is
        // bounded by the wrapper, and every caller synchronizes the queue before reuse/drop.
        list.append_host_to_device(dst, src, bytes, signal, &[])?;
    }
    list.close()
}

fn copy_d2h_sync(
    queue: &ze::CommandQueue<'_>,
    list: &ze::CommandList<'_>,
    dst: &mut ze::HostAllocation<'_>,
    src: &ze::DeviceAllocation<'_>,
    bytes: usize,
) -> ze::Result<()> {
    prepare_d2h_list(list, dst, src, bytes, None)?;
    queue.execute(&[list])?;
    queue.synchronize(QUEUE_SYNC_TIMEOUT_NS)
}

fn time_d2h_sync(
    queue: &ze::CommandQueue<'_>,
    list: &ze::CommandList<'_>,
    dst: &mut ze::HostAllocation<'_>,
    src: &ze::DeviceAllocation<'_>,
    bytes: usize,
) -> ze::Result<Duration> {
    prepare_d2h_list(list, dst, src, bytes, None)?;
    let started = Instant::now();
    queue.execute(&[list])?;
    queue.synchronize(QUEUE_SYNC_TIMEOUT_NS)?;
    Ok(started.elapsed())
}

fn prepare_d2h_list(
    list: &ze::CommandList<'_>,
    dst: &mut ze::HostAllocation<'_>,
    src: &ze::DeviceAllocation<'_>,
    bytes: usize,
    signal: Option<&ze::Event<'_>>,
) -> ze::Result<()> {
    list.reset()?;
    unsafe {
        // SAFETY: source and destination allocations outlive queue execution, byte count is
        // bounded by the wrapper, and every caller synchronizes the queue before reuse/drop.
        list.append_device_to_host(dst, src, bytes, signal, &[])?;
    }
    list.close()
}

fn time_d2d_sync(
    queue: &ze::CommandQueue<'_>,
    list: &ze::CommandList<'_>,
    dst: &ze::DeviceAllocation<'_>,
    src: &ze::DeviceAllocation<'_>,
    bytes: usize,
) -> ze::Result<Duration> {
    prepare_d2d_list(list, dst, src, bytes, None)?;
    let started = Instant::now();
    queue.execute(&[list])?;
    queue.synchronize(QUEUE_SYNC_TIMEOUT_NS)?;
    Ok(started.elapsed())
}

fn prepare_d2d_list(
    list: &ze::CommandList<'_>,
    dst: &ze::DeviceAllocation<'_>,
    src: &ze::DeviceAllocation<'_>,
    bytes: usize,
    signal: Option<&ze::Event<'_>>,
) -> ze::Result<()> {
    list.reset()?;
    unsafe {
        // SAFETY: source and destination allocations outlive queue execution, byte count is
        // bounded by the wrapper, and every caller synchronizes the queue before reuse/drop.
        list.append_device_to_device(dst, src, bytes, signal, &[])?;
    }
    list.close()
}

#[allow(clippy::too_many_arguments)]
fn copy_staged_sync(
    source_queue: &ze::CommandQueue<'_>,
    source_list: &ze::CommandList<'_>,
    destination_queue: &ze::CommandQueue<'_>,
    destination_list: &ze::CommandList<'_>,
    staging: &mut ze::HostAllocation<'_>,
    source: &ze::DeviceAllocation<'_>,
    destination: &ze::DeviceAllocation<'_>,
    bytes: usize,
) -> ze::Result<()> {
    prepare_staged_lists(
        source_list,
        destination_list,
        staging,
        source,
        destination,
        bytes,
    )?;
    source_queue.execute(&[source_list])?;
    source_queue.synchronize(QUEUE_SYNC_TIMEOUT_NS)?;
    destination_queue.execute(&[destination_list])?;
    destination_queue.synchronize(QUEUE_SYNC_TIMEOUT_NS)
}

#[allow(clippy::too_many_arguments)]
fn time_staged_sync(
    source_queue: &ze::CommandQueue<'_>,
    source_list: &ze::CommandList<'_>,
    destination_queue: &ze::CommandQueue<'_>,
    destination_list: &ze::CommandList<'_>,
    staging: &mut ze::HostAllocation<'_>,
    source: &ze::DeviceAllocation<'_>,
    destination: &ze::DeviceAllocation<'_>,
    bytes: usize,
) -> ze::Result<Duration> {
    prepare_staged_lists(
        source_list,
        destination_list,
        staging,
        source,
        destination,
        bytes,
    )?;
    let started = Instant::now();
    source_queue.execute(&[source_list])?;
    // The explicit staged sample is end-to-end: D2H must complete before the
    // H2D leg reads the pinned staging buffer.
    source_queue.synchronize(QUEUE_SYNC_TIMEOUT_NS)?;
    destination_queue.execute(&[destination_list])?;
    destination_queue.synchronize(QUEUE_SYNC_TIMEOUT_NS)?;
    Ok(started.elapsed())
}

fn prepare_staged_lists(
    source_list: &ze::CommandList<'_>,
    destination_list: &ze::CommandList<'_>,
    staging: &mut ze::HostAllocation<'_>,
    source: &ze::DeviceAllocation<'_>,
    destination: &ze::DeviceAllocation<'_>,
    bytes: usize,
) -> ze::Result<()> {
    prepare_d2h_list(source_list, staging, source, bytes, None)?;
    prepare_h2d_list(destination_list, destination, staging, bytes, None)
}

fn ze_fatal<T>(
    result: ze::Result<T>,
    phase: &'static str,
) -> std::result::Result<T, CaseExecutionError> {
    result.map_err(|error| {
        CaseExecutionError::Fatal(BenchmarkError::LevelZeroOperation { phase, error })
    })
}

fn duration_to_gb_s(bytes: u64, duration: Duration) -> Option<f64> {
    let seconds = duration.as_secs_f64();
    if seconds > 0.0 {
        Some(bytes as f64 / seconds / 1_000_000_000.0)
    } else {
        None
    }
}

fn fill_pattern(buffer: &mut [u8], seed: u8) {
    for (index, byte) in buffer.iter_mut().enumerate() {
        *byte = expected_byte(index, seed);
    }
}

fn verify_pattern(buffer: &[u8], seed: u8) -> std::result::Result<(), VerificationMismatch> {
    for (index, actual) in buffer.iter().copied().enumerate() {
        let expected = expected_byte(index, seed);
        if actual != expected {
            return Err(VerificationMismatch {
                offset: index,
                expected,
                actual,
            });
        }
    }
    Ok(())
}

fn verify_or_fail(
    buffer: &[u8],
    seed: u8,
    plan: &CasePlan,
) -> std::result::Result<(), CaseExecutionError> {
    verify_pattern(buffer, seed).map_err(|mismatch| {
        CaseExecutionError::Fatal(BenchmarkError::VerificationFailed {
            case: plan.label(),
            offset: mismatch.offset,
            expected: mismatch.expected,
            actual: mismatch.actual,
        })
    })
}

fn expected_byte(index: usize, seed: u8) -> u8 {
    let low = index.to_le_bytes()[0];
    let mixed = low.wrapping_mul(31).rotate_left(3);
    seed ^ mixed.wrapping_add(0x3d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_to_gb_s_uses_decimal_units() {
        assert_eq!(
            duration_to_gb_s(1_000_000_000, Duration::from_secs(2)),
            Some(0.5)
        );
    }

    #[test]
    fn duration_to_gb_s_rejects_zero_duration() {
        assert_eq!(duration_to_gb_s(1, Duration::ZERO), None);
    }

    #[test]
    fn pattern_verification_finds_first_bad_byte() {
        let mut bytes = vec![0; 64];
        fill_pattern(&mut bytes, PATTERN_SEED);
        bytes[17] ^= 1;

        assert_eq!(
            verify_pattern(&bytes, PATTERN_SEED),
            Err(VerificationMismatch {
                offset: 17,
                expected: expected_byte(17, PATTERN_SEED),
                actual: expected_byte(17, PATTERN_SEED) ^ 1,
            })
        );
    }

    #[test]
    fn joined_skip_reasons_preserves_all_reasons() {
        assert_eq!(joined_skip_reasons(&[]), None);
        assert_eq!(
            joined_skip_reasons(&["a".to_owned(), "b".to_owned()]),
            Some("a; b".to_owned())
        );
    }

    #[test]
    fn timestamp_ticks_use_valid_bit_wrap() {
        assert_eq!(elapsed_timestamp_ticks(10, 15, 8), 5);
        assert_eq!(elapsed_timestamp_ticks(250, 4, 8), 10);
        assert_eq!(elapsed_timestamp_ticks(u64::MAX - 2, 4, 0), 7);
        assert_eq!(timestamp_mask(64), u64::MAX);
        assert_eq!(timestamp_mask(65), u64::MAX);
    }
}
