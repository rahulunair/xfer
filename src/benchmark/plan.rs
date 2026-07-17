use crate::cli::{BenchOptions, TimingMode, TransferClass};
use crate::output::{
    AllocationKind, BenchCase, CaseOutcome, Endpoint, LinkInfo, Operation, QueueFlags,
    QueueGroupInfo,
};

use super::error::{BenchmarkError, Result};
use super::event::CaseId;
use super::topology::{DeviceRecord, QueueRecord, Topology};

pub(crate) const STAGED_DEVICE_TIMESTAMP_SKIP_REASON: &str = "device timestamp timing for explicit staged transfers is unsupported because the D2H and H2D legs can run on different device clock domains and cannot form one end-to-end device-time sample";

#[derive(Clone, Debug)]
pub(crate) struct QueueSelection {
    pub(crate) info: QueueGroupInfo,
    pub(crate) skip_reason: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct CasePlan {
    pub(crate) transfer_class: TransferClass,
    pub(crate) operation: Operation,
    pub(crate) source: Endpoint,
    pub(crate) destination: Endpoint,
    pub(crate) allocation: AllocationKind,
    pub(crate) queue: QueueGroupInfo,
    pub(crate) pcie_link: LinkInfo,
    pub(crate) execution: ExecutionPlan,
    pub(crate) skip_reasons: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExecutionPlan {
    HostToDevice { device: usize },
    DeviceToHost { device: usize },
    SameDevice { device: usize },
    Direct { source: usize, destination: usize },
    Staged { source: usize, destination: usize },
}

pub(crate) fn validate_filters(topology: &Topology, options: &BenchOptions) -> Result<()> {
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

pub(crate) fn plan_cases(topology: &Topology, options: &BenchOptions) -> Vec<CasePlan> {
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
    queue_selections_from_groups(
        device.index,
        &device
            .queues
            .iter()
            .map(|queue| queue.info.clone())
            .collect::<Vec<_>>(),
        options.queue_ordinal,
    )
}

fn queue_selections_from_groups(
    device_index: u32,
    queues: &[QueueGroupInfo],
    queue_ordinal: Option<u32>,
) -> Vec<QueueSelection> {
    if let Some(engine_id) = queue_ordinal {
        return match queues.iter().find(|queue| queue.ordinal == engine_id) {
            Some(queue) => vec![QueueSelection {
                info: queue.clone(),
                skip_reason: None,
            }],
            None => vec![QueueSelection {
                info: QueueGroupInfo {
                    ordinal: engine_id,
                    flags: QueueFlags::default(),
                },
                skip_reason: Some(format!(
                    "dev{device_index} has no usable engine {engine_id}"
                )),
            }],
        };
    }

    if queues.is_empty() {
        vec![QueueSelection {
            info: QueueGroupInfo {
                ordinal: 0,
                flags: QueueFlags::default(),
            },
            skip_reason: Some(format!("dev{device_index} has no usable queue groups")),
        }]
    } else {
        queues
            .iter()
            .map(|queue| QueueSelection {
                info: queue.clone(),
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
            "engine {} does not advertise copy capability",
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
    engine_id: u32,
    reasons: &mut Vec<String>,
) {
    match destination
        .queues
        .iter()
        .find(|queue| queue.info.ordinal == engine_id)
    {
        Some(queue) if queue.info.flags.copy => {}
        Some(_) => reasons.push(format!(
            "destination dev{} engine {engine_id} does not advertise copy capability",
            destination.index
        )),
        None => reasons.push(format!(
            "destination dev{} has no usable engine {engine_id}",
            destination.index
        )),
    }
}

pub(crate) fn joined_skip_reasons(reasons: &[String]) -> Option<String> {
    if reasons.is_empty() {
        None
    } else {
        Some(reasons.join("; "))
    }
}

impl CasePlan {
    pub(crate) fn into_case(self, options: &BenchOptions, outcome: CaseOutcome) -> BenchCase {
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

    pub(crate) fn label(&self) -> String {
        format!(
            "{} {} -> {} queue {}",
            self.transfer_class, self.source, self.destination, self.queue.ordinal
        )
    }

    pub(crate) fn case_id(&self, options: &BenchOptions) -> CaseId {
        CaseId::new(format!(
            "{}/{}-to-{}/{}/engine-{}/{}",
            self.transfer_class,
            self.source,
            self.destination,
            format_case_size(options.size_bytes),
            self.queue.ordinal,
            options.timing
        ))
    }
}

fn format_case_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;

    for (unit, factor) in [("GiB", GIB), ("MiB", MIB), ("KiB", KIB)] {
        if bytes >= factor && bytes % factor == 0 {
            return format!("{}{unit}", bytes / factor);
        }
    }

    format!("{bytes}B")
}

#[allow(dead_code)]
fn queue_records(infos: &[QueueGroupInfo]) -> Vec<QueueRecord> {
    infos
        .iter()
        .cloned()
        .map(|info| QueueRecord { info })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queue(ordinal: u32, copy: bool, compute: bool) -> QueueGroupInfo {
        QueueGroupInfo {
            ordinal,
            flags: QueueFlags { copy, compute },
        }
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
    fn explicit_queue_selection_never_falls_back() {
        let selections = queue_selections_from_groups(7, &[queue(1, true, false)], Some(3));

        assert_eq!(selections.len(), 1);
        assert_eq!(selections[0].info.ordinal, 3);
        assert_eq!(
            selections[0].skip_reason.as_deref(),
            Some("dev7 has no usable engine 3")
        );
    }

    #[test]
    fn case_id_uses_display_label_terms() {
        let plan = CasePlan {
            transfer_class: TransferClass::H2D,
            operation: Operation::HostToDevice,
            source: Endpoint::Host,
            destination: Endpoint::Device(0),
            allocation: AllocationKind::PinnedHost,
            queue: queue(2, true, false),
            pcie_link: LinkInfo::Unknown {
                reason: "test".to_owned(),
            },
            execution: ExecutionPlan::HostToDevice { device: 0 },
            skip_reasons: Vec::new(),
        };
        let options = BenchOptions {
            size_bytes: 256 * 1024 * 1024,
            ..BenchOptions::default()
        };

        assert_eq!(
            plan.case_id(&options).as_str(),
            "h2d/host-to-dev0/256MiB/engine-2/wall-clock"
        );
    }
}
