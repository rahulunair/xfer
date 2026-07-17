use crate::cli::{BenchMode, BenchOptions, TimingMode, TransferClass};
use crate::output::{
    AllocationKind, BenchCase, CaseOutcome, Endpoint, LinkInfo, Operation, QueueFlags,
    QueueGroupInfo, QueueStreamInfo,
};

use super::error::{BenchmarkError, Result};
use super::event::CaseId;
use super::topology::{DeviceRecord, Topology};

pub(crate) const STAGED_DEVICE_TIMESTAMP_SKIP_REASON: &str = "device timestamp timing for explicit staged transfers is unsupported because the D2H and H2D legs can run on different device clock domains and cannot form one end-to-end device-time sample";

#[derive(Clone, Debug)]
pub(crate) struct QueueSelection {
    pub(crate) selected_group: Option<QueueGroupInfo>,
    pub(crate) streams: Vec<QueueStreamInfo>,
    pub(crate) skip_reason: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct CasePlan {
    pub(crate) mode: BenchMode,
    pub(crate) selected_group: Option<QueueGroupInfo>,
    pub(crate) streams: Vec<QueueStreamInfo>,
    pub(crate) second_phase_streams: Vec<QueueStreamInfo>,
    pub(crate) verification_stream: Option<QueueStreamInfo>,
    pub(crate) transfer_class: TransferClass,
    pub(crate) operation: Operation,
    pub(crate) source: Endpoint,
    pub(crate) destination: Endpoint,
    pub(crate) allocation: AllocationKind,
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
    let groups = device
        .queues
        .iter()
        .map(|queue| queue.info.clone())
        .collect::<Vec<_>>();
    match options.mode {
        BenchMode::Single => {
            queue_selections_from_groups(device.index, &groups, options.queue_ordinal)
        }
        BenchMode::Saturation => vec![saturation_queue_selection(
            device.index,
            &groups,
            options.queue_ordinal,
        )],
    }
}

fn queue_selections_from_groups(
    device_index: u32,
    queues: &[QueueGroupInfo],
    queue_ordinal: Option<u32>,
) -> Vec<QueueSelection> {
    if let Some(group_ordinal) = queue_ordinal {
        return match queues.iter().find(|queue| queue.ordinal == group_ordinal) {
            Some(queue) => vec![QueueSelection {
                selected_group: Some(queue.clone()),
                streams: queue_streams(queue, true),
                skip_reason: None,
            }],
            None => vec![QueueSelection {
                selected_group: Some(QueueGroupInfo {
                    ordinal: group_ordinal,
                    flags: QueueFlags::default(),
                    queue_count: 0,
                }),
                streams: Vec::new(),
                skip_reason: Some(format!(
                    "dev{device_index} has no usable queue group {group_ordinal}"
                )),
            }],
        };
    }

    if queues.is_empty() {
        vec![QueueSelection {
            selected_group: Some(QueueGroupInfo {
                ordinal: 0,
                flags: QueueFlags::default(),
                queue_count: 0,
            }),
            streams: Vec::new(),
            skip_reason: Some(format!("dev{device_index} has no usable queue groups")),
        }]
    } else {
        queues
            .iter()
            .map(|queue| QueueSelection {
                selected_group: Some(queue.clone()),
                streams: queue_streams(queue, true),
                skip_reason: None,
            })
            .collect()
    }
}

fn saturation_queue_selection(
    device_index: u32,
    queues: &[QueueGroupInfo],
    queue_ordinal: Option<u32>,
) -> QueueSelection {
    if let Some(group_ordinal) = queue_ordinal {
        return queue_selections_from_groups(device_index, queues, Some(group_ordinal))
            .into_iter()
            .next()
            .expect("queue selection always returns one result")
            .with_all_group_streams();
    }

    let streams = queues
        .iter()
        .filter(|queue| queue.flags.copy)
        .flat_map(|queue| queue_streams(queue, false))
        .collect::<Vec<_>>();
    let skip_reason = streams
        .is_empty()
        .then(|| format!("dev{device_index} has no queue groups advertising copy capability"));
    QueueSelection {
        selected_group: None,
        streams,
        skip_reason,
    }
}

fn queue_streams(queue: &QueueGroupInfo, first_only: bool) -> Vec<QueueStreamInfo> {
    let count = if first_only {
        queue.queue_count.min(1)
    } else {
        queue.queue_count
    };
    (0..count)
        .map(|queue_index| QueueStreamInfo {
            group_ordinal: queue.ordinal,
            queue_index,
            flags: queue.flags,
        })
        .collect()
}

impl QueueSelection {
    fn with_all_group_streams(mut self) -> Self {
        if let Some(group) = &self.selected_group {
            self.streams = queue_streams(group, false);
        }
        self
    }
}

fn plan_h2d(topology: &Topology, options: &BenchOptions, device_index: usize) -> Vec<CasePlan> {
    let device = &topology.devices[device_index];
    queue_selections(device, options)
        .into_iter()
        .map(|queue| {
            let mut reasons = common_skip_reasons(&queue);
            check_device_allocation_size(options.size_bytes, &[device], &mut reasons);
            check_saturation_partition(options, &queue.streams, &mut reasons);
            CasePlan {
                mode: options.mode,
                selected_group: queue.selected_group,
                streams: queue.streams,
                second_phase_streams: Vec::new(),
                verification_stream: None,
                transfer_class: TransferClass::H2D,
                operation: Operation::HostToDevice,
                source: Endpoint::Host,
                destination: Endpoint::Device(device.index),
                allocation: AllocationKind::PinnedHost,
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
            check_saturation_partition(options, &queue.streams, &mut reasons);
            CasePlan {
                mode: options.mode,
                selected_group: queue.selected_group,
                streams: queue.streams,
                second_phase_streams: Vec::new(),
                verification_stream: None,
                transfer_class: TransferClass::D2H,
                operation: Operation::DeviceToHost,
                source: Endpoint::Device(device.index),
                destination: Endpoint::Host,
                allocation: AllocationKind::PinnedHost,
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
            check_saturation_partition(options, &queue.streams, &mut reasons);
            CasePlan {
                mode: options.mode,
                selected_group: queue.selected_group,
                streams: queue.streams,
                second_phase_streams: Vec::new(),
                verification_stream: None,
                transfer_class: TransferClass::D2DSameDevice,
                operation: Operation::SameDevice,
                source: Endpoint::Device(device.index),
                destination: Endpoint::Device(device.index),
                allocation: AllocationKind::Device,
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
            plans.extend(plan_cross_device_pair(
                topology,
                options,
                class,
                source_index,
                destination_index,
            ));
        }
    }

    plans
}

fn plan_cross_device_pair(
    topology: &Topology,
    options: &BenchOptions,
    class: TransferClass,
    source_index: usize,
    destination_index: usize,
) -> Vec<CasePlan> {
    let source = &topology.devices[source_index];
    queue_selections(source, options)
        .into_iter()
        .map(|queue| {
            plan_cross_device_case(
                topology,
                options,
                class,
                source_index,
                destination_index,
                queue,
            )
        })
        .collect()
}

fn plan_cross_device_case(
    topology: &Topology,
    options: &BenchOptions,
    class: TransferClass,
    source_index: usize,
    destination_index: usize,
    queue: QueueSelection,
) -> CasePlan {
    let source = &topology.devices[source_index];
    let destination = &topology.devices[destination_index];
    let mut reasons = common_skip_reasons(&queue);
    check_device_allocation_size(options.size_bytes, &[source, destination], &mut reasons);
    check_saturation_partition(options, &queue.streams, &mut reasons);

    let second_phase_streams = staged_destination_streams(
        class,
        destination,
        options,
        queue.selected_group.as_ref(),
        &mut reasons,
    );
    let verification_stream = if class == TransferClass::D2DDirect {
        destination_verification_stream(destination, &mut reasons)
    } else {
        None
    };
    let (operation, allocation, execution) = cross_device_semantics(
        topology,
        options,
        class,
        source_index,
        destination_index,
        &mut reasons,
    );

    CasePlan {
        mode: options.mode,
        selected_group: queue.selected_group,
        streams: queue.streams,
        second_phase_streams,
        verification_stream,
        transfer_class: class,
        operation,
        source: Endpoint::Device(source.index),
        destination: Endpoint::Device(destination.index),
        allocation,
        pcie_link: LinkInfo::Unknown {
            reason: "cross-device transfer has no single negotiated PCIe link".to_owned(),
        },
        execution,
        skip_reasons: reasons,
    }
}

fn staged_destination_streams(
    class: TransferClass,
    destination: &DeviceRecord,
    options: &BenchOptions,
    source_group: Option<&QueueGroupInfo>,
    reasons: &mut Vec<String>,
) -> Vec<QueueStreamInfo> {
    if class != TransferClass::D2DStaged {
        return Vec::new();
    }

    let selection = destination_phase_selection(destination, options, source_group);
    reasons.extend(common_skip_reasons(&selection));
    check_saturation_partition(options, &selection.streams, reasons);
    selection.streams
}

fn cross_device_semantics(
    topology: &Topology,
    options: &BenchOptions,
    class: TransferClass,
    source_index: usize,
    destination_index: usize,
    reasons: &mut Vec<String>,
) -> (Operation, AllocationKind, ExecutionPlan) {
    let source = &topology.devices[source_index];
    let destination = &topology.devices[destination_index];
    let route = topology.peer_route_between(source.index, destination.index);

    match class {
        TransferClass::D2DDirect => {
            if source.driver_index != destination.driver_index {
                reasons.push(
                    "direct cross-device copy requires both devices in one Level Zero driver/context; current level_zero.rs exposes context creation per driver"
                        .to_owned(),
                );
            }
            (
                Operation::Direct {
                    peer_access: topology.peer_access_between(source.index, destination.index),
                    route,
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
                Operation::ExplicitStaged { route },
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
    }
}

fn common_skip_reasons(queue: &QueueSelection) -> Vec<String> {
    let mut reasons = Vec::new();
    if let Some(reason) = &queue.skip_reason {
        reasons.push(reason.clone());
    } else if let Some(group) = &queue.selected_group {
        if !group.flags.copy {
            reasons.push(format!(
                "queue group {} does not advertise copy capability",
                group.ordinal
            ));
        }
    } else if queue.streams.is_empty() {
        reasons.push("no copy-capable queue streams selected".to_owned());
    }

    reasons
}

fn destination_phase_selection(
    destination: &DeviceRecord,
    options: &BenchOptions,
    source_group: Option<&QueueGroupInfo>,
) -> QueueSelection {
    match options.mode {
        BenchMode::Single => {
            let ordinal = source_group.map(|group| group.ordinal);
            queue_selections_from_groups(
                destination.index,
                &destination
                    .queues
                    .iter()
                    .map(|queue| queue.info.clone())
                    .collect::<Vec<_>>(),
                ordinal,
            )
            .into_iter()
            .next()
            .expect("queue selection always returns one result")
        }
        BenchMode::Saturation => saturation_queue_selection(
            destination.index,
            &destination
                .queues
                .iter()
                .map(|queue| queue.info.clone())
                .collect::<Vec<_>>(),
            options.queue_ordinal,
        ),
    }
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

fn check_saturation_partition(
    options: &BenchOptions,
    streams: &[QueueStreamInfo],
    reasons: &mut Vec<String>,
) {
    if options.mode == BenchMode::Saturation
        && !streams.is_empty()
        && options.size_bytes < streams.len() as u64
    {
        reasons.push(format!(
            "requested {} bytes cannot be divided into {} non-empty queue-stream regions",
            options.size_bytes,
            streams.len()
        ));
    }
}

fn destination_verification_stream(
    destination: &DeviceRecord,
    reasons: &mut Vec<String>,
) -> Option<QueueStreamInfo> {
    let stream = first_copy_stream(destination.queues.iter().map(|queue| &queue.info));
    if stream.is_none() {
        reasons.push(format!(
            "destination dev{} has no copy-capable engine for verification",
            destination.index
        ));
    }
    stream
}

fn first_copy_stream<'queue>(
    groups: impl IntoIterator<Item = &'queue QueueGroupInfo>,
) -> Option<QueueStreamInfo> {
    groups
        .into_iter()
        .find(|group| group.flags.copy && group.queue_count > 0)
        .map(|group| QueueStreamInfo {
            group_ordinal: group.ordinal,
            queue_index: 0,
            flags: group.flags,
        })
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
            mode: self.mode,
            selected_group: self.selected_group,
            streams: self.streams,
            second_phase_streams: self.second_phase_streams,
            verification_stream: self.verification_stream,
            transfer_class: self.transfer_class,
            operation: self.operation,
            source: self.source,
            destination: self.destination,
            byte_count: options.size_bytes,
            allocation: self.allocation,
            timing: options.timing,
            warmup: options.warmup,
            requested_samples: options.samples,
            pcie_link: self.pcie_link,
            outcome,
        }
    }

    pub(crate) fn label(&self) -> String {
        let queues = self.selected_group.as_ref().map_or_else(
            || "all copy queue groups".to_owned(),
            |group| format!("queue group {}", group.ordinal),
        );
        format!(
            "{} {} -> {} {queues}",
            self.transfer_class, self.source, self.destination
        )
    }

    pub(crate) fn single_group_ordinal(&self) -> u32 {
        self.selected_group
            .as_ref()
            .expect("single-transfer plans always select one queue group")
            .ordinal
    }

    pub(crate) fn case_id(&self, options: &BenchOptions) -> CaseId {
        let queue_scope = self.selected_group.as_ref().map_or_else(
            || "all-copy-groups".to_owned(),
            |group| format!("group-{}", group.ordinal),
        );
        CaseId::new(format!(
            "{}/{}-to-{}/{}/{queue_scope}/{}/{}-streams-{}",
            self.transfer_class,
            self.source,
            self.destination,
            format_case_size(options.size_bytes),
            options.timing,
            self.mode,
            self.streams.len()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn queue(ordinal: u32, copy: bool, compute: bool) -> QueueGroupInfo {
        QueueGroupInfo {
            ordinal,
            flags: QueueFlags { copy, compute },
            queue_count: 1,
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
        assert_eq!(
            selections[0]
                .selected_group
                .as_ref()
                .map(|group| group.ordinal),
            Some(3)
        );
        assert_eq!(
            selections[0].skip_reason.as_deref(),
            Some("dev7 has no usable queue group 3")
        );
    }

    #[test]
    fn saturation_selects_every_queue_in_copy_capable_groups() {
        let groups = [
            QueueGroupInfo {
                queue_count: 2,
                ..queue(0, true, true)
            },
            QueueGroupInfo {
                queue_count: 3,
                ..queue(1, false, true)
            },
            QueueGroupInfo {
                queue_count: 2,
                ..queue(2, true, false)
            },
        ];

        let selection = saturation_queue_selection(0, &groups, None);

        assert!(selection.selected_group.is_none());
        assert_eq!(
            selection
                .streams
                .iter()
                .map(|stream| (stream.group_ordinal, stream.queue_index))
                .collect::<Vec<_>>(),
            [(0, 0), (0, 1), (2, 0), (2, 1)]
        );
    }

    #[test]
    fn verification_engine_does_not_need_to_match_measured_engine_id() {
        let destination_groups = [queue(7, false, true), queue(9, true, false)];

        let selected = first_copy_stream(&destination_groups).expect("copy engine");

        assert_eq!(selected.group_ordinal, 9);
        assert_eq!(selected.queue_index, 0);
        assert!(selected.flags.copy);
    }

    #[test]
    fn case_id_uses_display_label_terms() {
        let plan = CasePlan {
            mode: BenchMode::Single,
            selected_group: Some(queue(2, true, false)),
            streams: vec![QueueStreamInfo {
                group_ordinal: 2,
                queue_index: 0,
                flags: QueueFlags {
                    copy: true,
                    compute: false,
                },
            }],
            second_phase_streams: Vec::new(),
            verification_stream: None,
            transfer_class: TransferClass::H2D,
            operation: Operation::HostToDevice,
            source: Endpoint::Host,
            destination: Endpoint::Device(0),
            allocation: AllocationKind::PinnedHost,
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
            "h2d/host-to-dev0/256MiB/group-2/wall-clock/single-streams-1"
        );
    }
}
