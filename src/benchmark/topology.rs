use crate::level_zero as ze;
use crate::output::{LinkInfo, PeerAccess, PeerAccessInfo, PeerRoute, QueueFlags, QueueGroupInfo};
use crate::pcie::{self, PcieLinkStatus, PcieLinkUnknown, PciePeerRoute};

use super::error::{BenchmarkError, Result};

const INTEL_VENDOR_ID: u32 = 0x8086;

#[derive(Clone, Debug)]
pub(crate) struct DeviceRecord {
    pub(crate) index: u32,
    pub(crate) driver_index: usize,
    pub(crate) device: ze::Device,
    pub(crate) properties: ze::DeviceProperties,
    pub(crate) pci: Option<pcie::PciAddress>,
    pub(crate) pci_address: Option<String>,
    pub(crate) pcie_link: LinkInfo,
    pub(crate) queues: Vec<QueueRecord>,
}

#[derive(Clone, Debug)]
pub(crate) struct QueueRecord {
    pub(crate) info: QueueGroupInfo,
}

#[derive(Debug)]
pub(crate) struct Topology {
    pub(crate) drivers: Vec<ze::Driver>,
    pub(crate) devices: Vec<DeviceRecord>,
    pub(crate) peer_access: Vec<PeerAccessInfo>,
}

pub(crate) fn discover_topology() -> Result<Topology> {
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

            let (pci, pci_address, pcie_link) = read_device_link(&device);
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
                        queue_count: queue.num_queues,
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
                pci,
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
                route: peer_route(from, to),
            });
        }
    }

    peers
}

fn read_device_link(device: &ze::Device) -> (Option<pcie::PciAddress>, Option<String>, LinkInfo) {
    let pci = match device.pci_address() {
        Ok(pci) => pci,
        Err(error) => {
            return (
                None,
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

    (address, address_text, link)
}

fn peer_route(source: &DeviceRecord, destination: &DeviceRecord) -> PeerRoute {
    let (Some(source_pci), Some(destination_pci)) = (source.pci, destination.pci) else {
        return PeerRoute::Unknown("Level Zero PCI address unavailable".to_owned());
    };

    match pcie::read_peer_route(source_pci, destination_pci) {
        Ok(PciePeerRoute::SameRootPort { root_port }) => PeerRoute::SameRootPort {
            root_port: root_port.to_string(),
        },
        Ok(PciePeerRoute::SharedUpstreamBridge { common_bridge }) => {
            PeerRoute::SharedUpstreamBridge {
                common_bridge: common_bridge.to_string(),
            }
        }
        Ok(PciePeerRoute::DifferentRootPorts {
            host_bridge,
            source_root_port,
            destination_root_port,
        }) => PeerRoute::DifferentRootPorts {
            host_bridge,
            source_root_port: source_root_port.to_string(),
            destination_root_port: destination_root_port.to_string(),
        },
        Ok(PciePeerRoute::CrossHostBridges {
            source_host_bridge,
            destination_host_bridge,
        }) => PeerRoute::CrossHostBridges {
            source_host_bridge,
            destination_host_bridge,
        },
        Err(reason) => PeerRoute::Unknown(pcie_unknown_reason(&reason)),
    }
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
        PcieLinkUnknown::UnreadableField { path, error, .. } => {
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

impl Topology {
    pub(crate) fn driver_for(&self, device: &DeviceRecord) -> Result<&ze::Driver> {
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

    pub(crate) fn peer_access_between(&self, from_device: u32, to_device: u32) -> PeerAccess {
        self.peer_access
            .iter()
            .find(|peer| peer.from_device == from_device && peer.to_device == to_device)
            .map_or_else(
                || PeerAccess::Unknown("zeDeviceCanAccessPeer was not queried".to_owned()),
                |peer| peer.access.clone(),
            )
    }

    pub(crate) fn peer_route_between(&self, from_device: u32, to_device: u32) -> PeerRoute {
        self.peer_access
            .iter()
            .find(|peer| peer.from_device == from_device && peer.to_device == to_device)
            .map_or_else(
                || PeerRoute::Unknown("PCIe peer topology was not queried".to_owned()),
                |peer| peer.route.clone(),
            )
    }
}
