use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::pcie::{self, PciAddress, PcieLinkUnknown};

const PCI_EXT_CAP_START: usize = 0x100;
const PCI_EXT_CAP_HEADER_LEN: usize = 4;
const PCI_EXT_CAP_HEADER_MIN_CONFIG_LEN: usize = PCI_EXT_CAP_START + PCI_EXT_CAP_HEADER_LEN;
const PCI_EXT_CAP_ID_ACS: u16 = 0x000d;
const PCI_EXT_CAP_NEXT_MASK: u32 = 0x0fff;
const PCI_EXT_CAP_NEXT_SHIFT: u32 = 20;
const ACS_CAPABILITY_OFFSET: usize = 0x04;
const ACS_CONTROL_OFFSET: usize = 0x06;
const ACS_BODY_LEN: usize = ACS_CONTROL_OFFSET + 2;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BridgePathEvidence {
    source: PciAddress,
    destination: PciAddress,
    bridges: Vec<BridgeEvidence>,
}

impl BridgePathEvidence {
    pub fn source(&self) -> PciAddress {
        self.source
    }

    pub fn destination(&self) -> PciAddress {
        self.destination
    }

    pub fn bridges(&self) -> &[BridgeEvidence] {
        &self.bridges
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BridgeEvidence {
    bridge: PciAddress,
    sysfs_path: PathBuf,
    result: std::result::Result<BridgeOutcome, ReadFailure>,
}

impl BridgeEvidence {
    pub fn bridge(&self) -> PciAddress {
        self.bridge
    }

    pub fn sysfs_path(&self) -> &Path {
        &self.sysfs_path
    }

    pub fn result(&self) -> &std::result::Result<BridgeOutcome, ReadFailure> {
        &self.result
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BridgeOutcome {
    /// A supported ACS redirect control bit is enabled on this bridge.
    RedirectObserved(Capability),
    /// No supported ACS redirect control bit is enabled on this bridge.
    ///
    /// This is not proof that peer traffic takes a direct physical route.
    NoRedirectObserved(Capability),
    /// The config read ended before the first PCI extended capability header.
    ExtendedConfigUnavailable { config_bytes: usize },
    /// Extended config was readable, but no ACS capability was present.
    NoCapability,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Capability {
    offset: usize,
    version: u8,
    raw_supported_bits: u16,
    raw_control: u16,
    supported: Flags,
    enabled: Flags,
}

impl Capability {
    pub fn offset(&self) -> usize {
        self.offset
    }

    pub fn version(&self) -> u8 {
        self.version
    }

    pub fn raw_capability(&self) -> u16 {
        self.raw_supported_bits
    }

    pub fn raw_control(&self) -> u16 {
        self.raw_control
    }

    pub fn supported(&self) -> Flags {
        self.supported
    }

    pub fn enabled(&self) -> Flags {
        self.enabled
    }

    pub fn redirect_enabled(&self) -> bool {
        self.enabled.p2p_request_redirect() || self.enabled.p2p_completion_redirect()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Flags {
    bits: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReadFailure {
    PermissionDenied {
        path: PathBuf,
        error: String,
    },
    NotFound {
        path: PathBuf,
    },
    Io {
        path: PathBuf,
        error: String,
    },
    Malformed {
        path: PathBuf,
        reason: MalformedConfig,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MalformedConfig {
    TruncatedExtendedCapabilityHeader {
        offset: usize,
        config_bytes: usize,
    },
    UnalignedNextPointer {
        offset: usize,
        next: usize,
    },
    BackwardNextPointer {
        offset: usize,
        next: usize,
    },
    OutOfBoundsNextPointer {
        offset: usize,
        next: usize,
        config_bytes: usize,
    },
    ExtendedCapabilityLoop {
        offset: usize,
    },
    TruncatedCapabilityBody {
        offset: usize,
        config_bytes: usize,
    },
}

pub fn read_bridge_path(
    source: PciAddress,
    destination: PciAddress,
) -> Result<BridgePathEvidence, PcieLinkUnknown> {
    read_bridge_path_from_sysfs(Path::new("/sys"), source, destination)
}

pub fn read_bridge_path_from_sysfs(
    sysfs_root: impl AsRef<Path>,
    source: PciAddress,
    destination: PciAddress,
) -> Result<BridgePathEvidence, PcieLinkUnknown> {
    let bridges = pcie::pci_bridge_ancestor_union_from_sysfs(sysfs_root, source, destination)?
        .into_iter()
        .map(|ancestor| {
            let result = read_bridge_config(&ancestor.sysfs_path);
            BridgeEvidence {
                bridge: ancestor.address,
                sysfs_path: ancestor.sysfs_path,
                result,
            }
        })
        .collect();

    Ok(BridgePathEvidence {
        source,
        destination,
        bridges,
    })
}

pub fn parse_config(config: &[u8]) -> Result<BridgeOutcome, MalformedConfig> {
    if config.len() < PCI_EXT_CAP_HEADER_MIN_CONFIG_LEN {
        return Ok(BridgeOutcome::ExtendedConfigUnavailable {
            config_bytes: config.len(),
        });
    }

    match find_acs_capability(config)? {
        Some((offset, version)) => {
            let capability = parse_acs_capability(config, offset, version)?;
            if capability.redirect_enabled() {
                Ok(BridgeOutcome::RedirectObserved(capability))
            } else {
                Ok(BridgeOutcome::NoRedirectObserved(capability))
            }
        }
        None => Ok(BridgeOutcome::NoCapability),
    }
}

fn read_bridge_config(bridge_sysfs_path: &Path) -> std::result::Result<BridgeOutcome, ReadFailure> {
    let config_path = bridge_sysfs_path.join("config");
    let config = fs::read(&config_path).map_err(|error| io_failure(config_path.clone(), &error))?;
    parse_config(&config).map_err(|reason| ReadFailure::Malformed {
        path: config_path,
        reason,
    })
}

fn find_acs_capability(config: &[u8]) -> Result<Option<(usize, u8)>, MalformedConfig> {
    let mut offset = PCI_EXT_CAP_START;
    let mut seen = Vec::new();

    loop {
        if seen.contains(&offset) {
            return Err(MalformedConfig::ExtendedCapabilityLoop { offset });
        }
        seen.push(offset);

        let header = read_u32_le(config, offset)?;
        let id = (header & 0xffff) as u16;
        let version = ((header >> 16) & 0x0f) as u8;
        if id == PCI_EXT_CAP_ID_ACS {
            return Ok(Some((offset, version)));
        }

        let next = ((header >> PCI_EXT_CAP_NEXT_SHIFT) & PCI_EXT_CAP_NEXT_MASK) as usize;
        if next == 0 {
            return Ok(None);
        }

        validate_next_pointer(config.len(), offset, next, &seen)?;
        offset = next;
    }
}

fn validate_next_pointer(
    config_bytes: usize,
    offset: usize,
    next: usize,
    seen: &[usize],
) -> Result<(), MalformedConfig> {
    if next % 4 != 0 {
        return Err(MalformedConfig::UnalignedNextPointer { offset, next });
    }
    if next < PCI_EXT_CAP_START
        || next
            .checked_add(PCI_EXT_CAP_HEADER_LEN)
            .is_none_or(|end| end > config_bytes)
    {
        return Err(MalformedConfig::OutOfBoundsNextPointer {
            offset,
            next,
            config_bytes,
        });
    }
    if seen.contains(&next) {
        return Err(MalformedConfig::ExtendedCapabilityLoop { offset: next });
    }
    if next <= offset {
        return Err(MalformedConfig::BackwardNextPointer { offset, next });
    }

    Ok(())
}

fn parse_acs_capability(
    config: &[u8],
    offset: usize,
    version: u8,
) -> Result<Capability, MalformedConfig> {
    if offset
        .checked_add(ACS_BODY_LEN)
        .is_none_or(|end| end > config.len())
    {
        return Err(MalformedConfig::TruncatedCapabilityBody {
            offset,
            config_bytes: config.len(),
        });
    }

    let raw_supported_bits = read_u16_le(config, offset + ACS_CAPABILITY_OFFSET)?;
    let raw_control = read_u16_le(config, offset + ACS_CONTROL_OFFSET)?;
    let supported = Flags::from_bits(raw_supported_bits);
    let enabled = Flags::from_bits(raw_supported_bits & raw_control);

    Ok(Capability {
        offset,
        version,
        raw_supported_bits,
        raw_control,
        supported,
        enabled,
    })
}

fn read_u32_le(config: &[u8], offset: usize) -> Result<u32, MalformedConfig> {
    let bytes = config.get(offset..offset + 4).ok_or(
        MalformedConfig::TruncatedExtendedCapabilityHeader {
            offset,
            config_bytes: config.len(),
        },
    )?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_u16_le(config: &[u8], offset: usize) -> Result<u16, MalformedConfig> {
    let bytes = config
        .get(offset..offset + 2)
        .ok_or(MalformedConfig::TruncatedCapabilityBody {
            offset,
            config_bytes: config.len(),
        })?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn io_failure(path: PathBuf, error: &io::Error) -> ReadFailure {
    match error.kind() {
        io::ErrorKind::PermissionDenied => ReadFailure::PermissionDenied {
            path,
            error: error.to_string(),
        },
        io::ErrorKind::NotFound => ReadFailure::NotFound { path },
        _ => ReadFailure::Io {
            path,
            error: error.to_string(),
        },
    }
}

impl Flags {
    const SOURCE_VALIDATION: u16 = 1 << 0;
    const TRANSLATION_BLOCKING: u16 = 1 << 1;
    const P2P_REQUEST_REDIRECT: u16 = 1 << 2;
    const P2P_COMPLETION_REDIRECT: u16 = 1 << 3;
    const UPSTREAM_FORWARDING: u16 = 1 << 4;
    const P2P_EGRESS_CONTROL: u16 = 1 << 5;
    const DIRECT_TRANSLATED_P2P: u16 = 1 << 6;
    const DECODED_MASK: u16 = Self::SOURCE_VALIDATION
        | Self::TRANSLATION_BLOCKING
        | Self::P2P_REQUEST_REDIRECT
        | Self::P2P_COMPLETION_REDIRECT
        | Self::UPSTREAM_FORWARDING
        | Self::P2P_EGRESS_CONTROL
        | Self::DIRECT_TRANSLATED_P2P;

    pub fn source_validation(self) -> bool {
        self.has(Self::SOURCE_VALIDATION)
    }

    pub fn translation_blocking(self) -> bool {
        self.has(Self::TRANSLATION_BLOCKING)
    }

    pub fn p2p_request_redirect(self) -> bool {
        self.has(Self::P2P_REQUEST_REDIRECT)
    }

    pub fn p2p_completion_redirect(self) -> bool {
        self.has(Self::P2P_COMPLETION_REDIRECT)
    }

    pub fn upstream_forwarding(self) -> bool {
        self.has(Self::UPSTREAM_FORWARDING)
    }

    pub fn p2p_egress_control(self) -> bool {
        self.has(Self::P2P_EGRESS_CONTROL)
    }

    pub fn direct_translated_p2p(self) -> bool {
        self.has(Self::DIRECT_TRANSLATED_P2P)
    }

    pub fn bits(self) -> u16 {
        self.bits
    }

    fn from_bits(bits: u16) -> Self {
        Self {
            bits: bits & Self::DECODED_MASK,
        }
    }

    fn has(self, mask: u16) -> bool {
        self.bits & mask != 0
    }
}

impl fmt::Display for ReadFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PermissionDenied { path, error } => {
                write!(f, "permission denied reading {}: {error}", path.display())
            }
            Self::NotFound { path } => write!(f, "missing {}", path.display()),
            Self::Io { path, error } => write!(f, "cannot read {}: {error}", path.display()),
            Self::Malformed { path, reason } => {
                write!(f, "malformed PCI config in {}: {reason}", path.display())
            }
        }
    }
}

impl fmt::Display for MalformedConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TruncatedExtendedCapabilityHeader {
                offset,
                config_bytes,
            } => write!(
                f,
                "extended capability header at 0x{offset:x} exceeds {config_bytes} config bytes"
            ),
            Self::UnalignedNextPointer { offset, next } => write!(
                f,
                "extended capability at 0x{offset:x} has unaligned next pointer 0x{next:x}"
            ),
            Self::BackwardNextPointer { offset, next } => write!(
                f,
                "extended capability at 0x{offset:x} has non-progressing next pointer 0x{next:x}"
            ),
            Self::OutOfBoundsNextPointer {
                offset,
                next,
                config_bytes,
            } => write!(
                f,
                "extended capability at 0x{offset:x} points to 0x{next:x}, outside {config_bytes} config bytes"
            ),
            Self::ExtendedCapabilityLoop { offset } => {
                write!(f, "extended capability list loops to 0x{offset:x}")
            }
            Self::TruncatedCapabilityBody {
                offset,
                config_bytes,
            } => write!(
                f,
                "ACS capability at 0x{offset:x} exceeds {config_bytes} config bytes"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::io::{Error, ErrorKind, Write};
    use std::time::{SystemTime, UNIX_EPOCH};

    const SYSFS_PCI_DEVICES: &str = "bus/pci/devices";

    #[test]
    fn treats_64_byte_config_as_extended_config_unavailable() {
        assert_eq!(
            parse_config(&[0; 64]),
            Ok(BridgeOutcome::ExtendedConfigUnavailable { config_bytes: 64 })
        );
    }

    #[test]
    fn reports_absent_acs_capability() {
        let mut config = config(PCI_EXT_CAP_HEADER_MIN_CONFIG_LEN);
        write_ext_header(&mut config, PCI_EXT_CAP_START, 0x0001, 1, 0);

        assert_eq!(parse_config(&config), Ok(BridgeOutcome::NoCapability));
    }

    #[test]
    fn reports_redirect_when_redirect_control_is_supported_and_enabled() {
        let mut config = config(PCI_EXT_CAP_HEADER_MIN_CONFIG_LEN + ACS_BODY_LEN);
        write_ext_header(&mut config, PCI_EXT_CAP_START, PCI_EXT_CAP_ID_ACS, 2, 0);
        write_u16(
            &mut config,
            PCI_EXT_CAP_START + ACS_CAPABILITY_OFFSET,
            0x004c,
        );
        write_u16(&mut config, PCI_EXT_CAP_START + ACS_CONTROL_OFFSET, 0x0004);

        let Ok(BridgeOutcome::RedirectObserved(capability)) = parse_config(&config) else {
            panic!("expected redirect-observed ACS capability");
        };

        assert_eq!(capability.offset(), PCI_EXT_CAP_START);
        assert_eq!(capability.version(), 2);
        assert_eq!(capability.raw_capability(), 0x004c);
        assert_eq!(capability.raw_control(), 0x0004);
        assert!(capability.supported().p2p_request_redirect());
        assert!(capability.supported().p2p_completion_redirect());
        assert!(capability.supported().direct_translated_p2p());
        assert!(capability.enabled().p2p_request_redirect());
        assert!(!capability.enabled().p2p_completion_redirect());
    }

    #[test]
    fn reports_acs_without_redirect_as_no_redirect_observed() {
        let mut config = config(PCI_EXT_CAP_HEADER_MIN_CONFIG_LEN + ACS_BODY_LEN);
        write_ext_header(&mut config, PCI_EXT_CAP_START, PCI_EXT_CAP_ID_ACS, 1, 0);
        write_u16(
            &mut config,
            PCI_EXT_CAP_START + ACS_CAPABILITY_OFFSET,
            0x0013,
        );
        write_u16(&mut config, PCI_EXT_CAP_START + ACS_CONTROL_OFFSET, 0x0011);

        let Ok(BridgeOutcome::NoRedirectObserved(capability)) = parse_config(&config) else {
            panic!("expected ACS without redirect");
        };

        assert!(capability.supported().source_validation());
        assert!(capability.supported().translation_blocking());
        assert!(capability.supported().upstream_forwarding());
        assert!(capability.enabled().source_validation());
        assert!(capability.enabled().upstream_forwarding());
        assert!(!capability.redirect_enabled());
    }

    #[test]
    fn zero_control_never_reports_supported_redirect_as_enabled() {
        let mut config = config(PCI_EXT_CAP_HEADER_MIN_CONFIG_LEN + ACS_BODY_LEN);
        write_ext_header(&mut config, PCI_EXT_CAP_START, PCI_EXT_CAP_ID_ACS, 1, 0);
        write_u16(
            &mut config,
            PCI_EXT_CAP_START + ACS_CAPABILITY_OFFSET,
            0x001d,
        );
        write_u16(&mut config, PCI_EXT_CAP_START + ACS_CONTROL_OFFSET, 0x0000);

        let Ok(BridgeOutcome::NoRedirectObserved(capability)) = parse_config(&config) else {
            panic!("zero ACSCtl must not report redirect enabled");
        };
        assert_eq!(capability.raw_control(), 0);
        assert!(!capability.redirect_enabled());
    }

    #[test]
    fn egress_control_alone_is_not_reported_as_redirect() {
        let mut config = config(PCI_EXT_CAP_HEADER_MIN_CONFIG_LEN + ACS_BODY_LEN);
        write_ext_header(&mut config, PCI_EXT_CAP_START, PCI_EXT_CAP_ID_ACS, 1, 0);
        write_u16(
            &mut config,
            PCI_EXT_CAP_START + ACS_CAPABILITY_OFFSET,
            0x0020,
        );
        write_u16(&mut config, PCI_EXT_CAP_START + ACS_CONTROL_OFFSET, 0x0020);

        let Ok(BridgeOutcome::NoRedirectObserved(capability)) = parse_config(&config) else {
            panic!("egress control alone must not be reported as redirect");
        };

        assert!(capability.enabled().p2p_egress_control());
        assert!(!capability.redirect_enabled());
    }

    #[test]
    fn ignores_enabled_control_bits_that_are_not_supported() {
        let mut config = config(PCI_EXT_CAP_HEADER_MIN_CONFIG_LEN + ACS_BODY_LEN);
        write_ext_header(&mut config, PCI_EXT_CAP_START, PCI_EXT_CAP_ID_ACS, 1, 0);
        write_u16(
            &mut config,
            PCI_EXT_CAP_START + ACS_CAPABILITY_OFFSET,
            0x0000,
        );
        write_u16(&mut config, PCI_EXT_CAP_START + ACS_CONTROL_OFFSET, 0x002c);

        let Ok(BridgeOutcome::NoRedirectObserved(capability)) = parse_config(&config) else {
            panic!("expected unsupported control bits to be ignored");
        };

        assert_eq!(capability.raw_control(), 0x002c);
        assert_eq!(capability.supported(), Flags::default());
        assert_eq!(capability.enabled(), Flags::default());
    }

    #[test]
    fn rejects_extended_capability_loop() {
        let mut config = config(0x10c);
        write_ext_header(&mut config, 0x100, 0x0001, 1, 0x108);
        write_ext_header(&mut config, 0x108, 0x0002, 1, 0x100);

        assert_eq!(
            parse_config(&config),
            Err(MalformedConfig::ExtendedCapabilityLoop { offset: 0x100 })
        );
    }

    #[test]
    fn rejects_unaligned_next_pointer() {
        let mut config = config(PCI_EXT_CAP_HEADER_MIN_CONFIG_LEN);
        write_ext_header(&mut config, 0x100, 0x0001, 1, 0x102);

        assert_eq!(
            parse_config(&config),
            Err(MalformedConfig::UnalignedNextPointer {
                offset: 0x100,
                next: 0x102,
            })
        );
    }

    #[test]
    fn rejects_backward_next_pointer() {
        let mut config = config(0x10c);
        write_ext_header(&mut config, 0x100, 0x0001, 1, 0x108);
        write_ext_header(&mut config, 0x108, 0x0002, 1, 0x104);

        assert_eq!(
            parse_config(&config),
            Err(MalformedConfig::BackwardNextPointer {
                offset: 0x108,
                next: 0x104,
            })
        );
    }

    #[test]
    fn rejects_out_of_bounds_next_pointer() {
        let mut config = config(PCI_EXT_CAP_HEADER_MIN_CONFIG_LEN);
        write_ext_header(&mut config, 0x100, 0x0001, 1, 0x200);

        assert_eq!(
            parse_config(&config),
            Err(MalformedConfig::OutOfBoundsNextPointer {
                offset: 0x100,
                next: 0x200,
                config_bytes: PCI_EXT_CAP_HEADER_MIN_CONFIG_LEN,
            })
        );
    }

    #[test]
    fn rejects_truncated_acs_body() {
        let mut config = config(0x106);
        write_ext_header(&mut config, 0x100, PCI_EXT_CAP_ID_ACS, 1, 0);

        assert_eq!(
            parse_config(&config),
            Err(MalformedConfig::TruncatedCapabilityBody {
                offset: 0x100,
                config_bytes: 0x106,
            })
        );
    }

    #[test]
    fn maps_io_error_kinds_without_checking_process_identity() {
        let path = PathBuf::from("/sys/bus/pci/devices/0000:00:00.0/config");

        assert_eq!(
            io_failure(
                path.clone(),
                &Error::new(ErrorKind::PermissionDenied, "denied"),
            ),
            ReadFailure::PermissionDenied {
                path: path.clone(),
                error: "denied".to_owned(),
            }
        );
        assert_eq!(
            io_failure(path.clone(), &Error::new(ErrorKind::NotFound, "missing")),
            ReadFailure::NotFound { path: path.clone() }
        );
        assert_eq!(
            io_failure(path.clone(), &Error::other("other")),
            ReadFailure::Io {
                path,
                error: "other".to_owned(),
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn reads_deterministic_deduplicated_bridge_union_from_fake_sysfs() {
        let sysfs = TestSysfs::new();
        let root = PciAddress::new(0, 0x10, 1, 0).expect("root");
        let shared = PciAddress::new(0, 0x11, 0, 0).expect("shared");
        let source_port = PciAddress::new(0, 0x12, 2, 0).expect("source port");
        let destination_port = PciAddress::new(0, 0x12, 1, 0).expect("destination port");
        let source = PciAddress::new(0, 0x13, 0, 0).expect("source");
        let destination = PciAddress::new(0, 0x14, 0, 0).expect("destination");

        let source_chain = sysfs.add_nested_device(source, &[root, shared, source_port, source]);
        let destination_chain =
            sysfs.add_nested_device(destination, &[root, shared, destination_port, destination]);

        write_config(&source_chain[0].1, &config_without_acs());
        write_config(&source_chain[1].1, &config_with_acs(0x0004, 0x0004));
        write_config(&source_chain[2].1, &config_with_acs(0x0010, 0x0010));
        write_config(&destination_chain[2].1, &config_with_acs(0x0020, 0x0000));

        let forward =
            read_bridge_path_from_sysfs(&sysfs.root, source, destination).expect("read ACS path");
        let reverse =
            read_bridge_path_from_sysfs(&sysfs.root, destination, source).expect("read ACS path");
        let addresses = forward
            .bridges()
            .iter()
            .map(BridgeEvidence::bridge)
            .collect::<Vec<_>>();

        assert_eq!(addresses, vec![root, shared, destination_port, source_port]);
        assert_eq!(
            reverse
                .bridges()
                .iter()
                .map(BridgeEvidence::bridge)
                .collect::<Vec<_>>(),
            addresses
        );
        assert_eq!(forward.source(), source);
        assert_eq!(forward.destination(), destination);
        assert_eq!(forward.bridges().len(), 4);
        assert!(matches!(
            forward.bridges()[1].result(),
            Ok(BridgeOutcome::RedirectObserved(_))
        ));
        assert!(matches!(
            forward.bridges()[2].result(),
            Ok(BridgeOutcome::NoRedirectObserved(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn keeps_one_bridge_failure_without_erasing_other_bridge_results() {
        let sysfs = TestSysfs::new();
        let root = PciAddress::new(0, 0x20, 1, 0).expect("root");
        let source_port = PciAddress::new(0, 0x21, 1, 0).expect("source port");
        let destination_port = PciAddress::new(0, 0x21, 2, 0).expect("destination port");
        let source = PciAddress::new(0, 0x22, 0, 0).expect("source");
        let destination = PciAddress::new(0, 0x23, 0, 0).expect("destination");

        let source_chain = sysfs.add_nested_device(source, &[root, source_port, source]);
        let destination_chain =
            sysfs.add_nested_device(destination, &[root, destination_port, destination]);

        write_config(&source_chain[0].1, &config_without_acs());
        write_config(&source_chain[1].1, &config_with_acs(0x0004, 0x0004));
        let mut truncated_acs = config(0x106);
        write_ext_header(&mut truncated_acs, 0x100, PCI_EXT_CAP_ID_ACS, 1, 0);
        write_config(&destination_chain[1].1, &truncated_acs);

        let evidence =
            read_bridge_path_from_sysfs(&sysfs.root, source, destination).expect("read ACS path");
        let results = evidence
            .bridges()
            .iter()
            .map(BridgeEvidence::result)
            .collect::<Vec<_>>();

        assert!(matches!(results[0], Ok(BridgeOutcome::NoCapability)));
        assert!(matches!(results[1], Ok(BridgeOutcome::RedirectObserved(_))));
        assert!(matches!(
            results[2],
            Err(ReadFailure::Malformed {
                reason: MalformedConfig::TruncatedCapabilityBody {
                    offset: 0x100,
                    config_bytes: 0x106,
                },
                ..
            })
        ));
    }

    fn config(bytes: usize) -> Vec<u8> {
        vec![0; bytes]
    }

    fn config_without_acs() -> Vec<u8> {
        let mut config = config(PCI_EXT_CAP_HEADER_MIN_CONFIG_LEN);
        write_ext_header(&mut config, PCI_EXT_CAP_START, 0x0001, 1, 0);
        config
    }

    fn config_with_acs(raw_capability: u16, raw_control: u16) -> Vec<u8> {
        let mut config = config(PCI_EXT_CAP_HEADER_MIN_CONFIG_LEN + ACS_BODY_LEN);
        write_ext_header(&mut config, PCI_EXT_CAP_START, PCI_EXT_CAP_ID_ACS, 1, 0);
        write_u16(
            &mut config,
            PCI_EXT_CAP_START + ACS_CAPABILITY_OFFSET,
            raw_capability,
        );
        write_u16(
            &mut config,
            PCI_EXT_CAP_START + ACS_CONTROL_OFFSET,
            raw_control,
        );
        config
    }

    fn write_ext_header(config: &mut [u8], offset: usize, id: u16, version: u8, next: usize) {
        let header = u32::from(id)
            | (u32::from(version & 0x0f) << 16)
            | ((next as u32 & PCI_EXT_CAP_NEXT_MASK) << PCI_EXT_CAP_NEXT_SHIFT);
        config[offset..offset + 4].copy_from_slice(&header.to_le_bytes());
    }

    fn write_u16(config: &mut [u8], offset: usize, value: u16) {
        config[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn write_config(device_path: &Path, config: &[u8]) {
        let mut file = File::create(device_path.join("config")).expect("create config");
        file.write_all(config).expect("write config");
    }

    #[cfg(unix)]
    struct TestSysfs {
        root: PathBuf,
    }

    #[cfg(unix)]
    impl TestSysfs {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock before Unix epoch")
                .as_nanos();
            let root =
                std::env::temp_dir().join(format!("xfer-acs-test-{}-{nonce}", std::process::id()));
            fs::create_dir_all(root.join(SYSFS_PCI_DEVICES)).expect("create fake sysfs");
            Self { root }
        }

        fn device_dir(&self, address: PciAddress) -> PathBuf {
            self.root.join(SYSFS_PCI_DEVICES).join(address.sysfs_name())
        }

        fn add_nested_device(
            &self,
            endpoint: PciAddress,
            chain: &[PciAddress],
        ) -> Vec<(PciAddress, PathBuf)> {
            use std::os::unix::fs::symlink;

            assert_eq!(chain.last(), Some(&endpoint));

            let mut current = self
                .root
                .join("devices")
                .join(format!("pci{:04x}:{:02x}", chain[0].domain, chain[0].bus));
            fs::create_dir_all(&current).expect("create fake PCI domain root");

            let mut paths = Vec::new();
            for address in chain {
                current = current.join(address.sysfs_name());
                fs::create_dir_all(&current).expect("create fake canonical PCI path");
                paths.push((*address, current.clone()));
            }

            symlink(&current, self.device_dir(endpoint)).expect("create fake bus PCI symlink");
            paths
        }
    }

    #[cfg(unix)]
    impl Drop for TestSysfs {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}
