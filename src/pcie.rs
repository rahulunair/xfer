use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

const SYSFS_PCI_DEVICES: &str = "bus/pci/devices";
const SYSFS_PCI_SLOTS: &str = "bus/pci/slots";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PciAddress {
    pub domain: u16,
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

impl PciAddress {
    pub fn new(domain: u32, bus: u32, device: u32, function: u32) -> Option<Self> {
        if domain > 0xffff || bus > 0xff || device > 0x1f || function > 0x7 {
            return None;
        }

        Some(Self {
            domain: domain as u16,
            bus: bus as u8,
            device: device as u8,
            function: function as u8,
        })
    }

    pub fn sysfs_name(self) -> String {
        format!(
            "{:04x}:{:02x}:{:02x}.{}",
            self.domain, self.bus, self.device, self.function
        )
    }
}

impl fmt::Display for PciAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.sysfs_name())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PcieGeneration {
    Gen1,
    Gen2,
    Gen3,
    Gen4,
    Gen5,
    Gen6,
    Gen7,
}

impl PcieGeneration {
    pub fn number(self) -> u8 {
        match self {
            Self::Gen1 => 1,
            Self::Gen2 => 2,
            Self::Gen3 => 3,
            Self::Gen4 => 4,
            Self::Gen5 => 5,
            Self::Gen6 => 6,
            Self::Gen7 => 7,
        }
    }

    fn gt_per_second_milli(self) -> u32 {
        match self {
            Self::Gen1 => 2_500,
            Self::Gen2 => 5_000,
            Self::Gen3 => 8_000,
            Self::Gen4 => 16_000,
            Self::Gen5 => 32_000,
            Self::Gen6 => 64_000,
            Self::Gen7 => 128_000,
        }
    }

    fn encoding_efficiency(self) -> (u32, u32) {
        match self {
            Self::Gen1 | Self::Gen2 => (8, 10),
            Self::Gen3 | Self::Gen4 | Self::Gen5 => (128, 130),
            Self::Gen6 | Self::Gen7 => (242, 256),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LinkSpeed {
    pub generation: PcieGeneration,
    pub gt_per_second_milli: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PcieLink {
    pub address: PciAddress,
    pub sysfs_path: PathBuf,
    pub speed: LinkSpeed,
    pub width: u16,
    pub theoretical_gb_s: f64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PcieLinkUnknown {
    InvalidAddress,
    MissingDevice(PathBuf),
    UnreliableDevicePath(PathBuf),
    UnreadableField { path: PathBuf, error: String },
    UnrecognizedSpeed(String),
    UnrecognizedWidth(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum PcieLinkStatus {
    Known(PcieLink),
    Unknown(PcieLinkUnknown),
}

pub fn read_link(address: PciAddress) -> PcieLinkStatus {
    read_link_from_sysfs(Path::new("/sys"), address)
}

pub fn read_link_for_level_zero_pci(
    domain: u32,
    bus: u32,
    device: u32,
    function: u32,
) -> PcieLinkStatus {
    read_link_for_level_zero_pci_from_sysfs(Path::new("/sys"), domain, bus, device, function)
}

pub fn read_link_for_level_zero_pci_from_sysfs(
    sysfs_root: impl AsRef<Path>,
    domain: u32,
    bus: u32,
    device: u32,
    function: u32,
) -> PcieLinkStatus {
    let Some(address) = PciAddress::new(domain, bus, device, function) else {
        return PcieLinkStatus::Unknown(PcieLinkUnknown::InvalidAddress);
    };

    read_link_from_sysfs(sysfs_root, address)
}

pub fn read_link_from_sysfs(sysfs_root: impl AsRef<Path>, address: PciAddress) -> PcieLinkStatus {
    let resolved_device = match resolve_pci_device(sysfs_root.as_ref(), address) {
        Ok(device) => device,
        Err(reason) => return PcieLinkStatus::Unknown(reason),
    };

    let link_path = match negotiated_link_source_path(sysfs_root.as_ref(), &resolved_device) {
        Ok(path) => path,
        Err(reason) => return PcieLinkStatus::Unknown(reason),
    };

    let (speed, width) = match read_link_fields(&link_path) {
        Ok(link_fields) => link_fields,
        Err(reason) => return PcieLinkStatus::Unknown(reason),
    };

    PcieLinkStatus::Known(PcieLink {
        address,
        sysfs_path: link_path,
        speed,
        width,
        theoretical_gb_s: theoretical_payload_gb_s(speed.generation, width),
    })
}

pub fn pci_device_path(
    sysfs_root: impl AsRef<Path>,
    address: PciAddress,
) -> Result<PathBuf, PcieLinkUnknown> {
    resolve_pci_device(sysfs_root, address).map(|device| device.device_sysfs)
}

pub fn parse_link_speed(text: &str) -> Option<LinkSpeed> {
    let mut fields = text.trim().split_whitespace();
    let value = fields.next()?;
    let units = fields.next()?;

    if !units.eq_ignore_ascii_case("GT/s") {
        return None;
    }

    let gt_per_second_milli = parse_decimal_milli(value)?;
    let generation = generation_from_gt_per_second_milli(gt_per_second_milli)?;

    Some(LinkSpeed {
        generation,
        gt_per_second_milli,
    })
}

pub fn parse_link_width(text: &str) -> Option<u16> {
    let trimmed = text.trim();
    let digits = trimmed
        .strip_prefix('x')
        .or_else(|| trimmed.strip_prefix('X'))
        .unwrap_or(trimmed);

    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }

    let width = digits.parse::<u16>().ok()?;
    if (1..=64).contains(&width) {
        Some(width)
    } else {
        None
    }
}

pub fn theoretical_payload_gb_s(generation: PcieGeneration, width: u16) -> f64 {
    if width == 0 {
        return 0.0;
    }

    let (encoding_numerator, encoding_denominator) = generation.encoding_efficiency();
    let per_lane_gb_s = f64::from(generation.gt_per_second_milli()) * f64::from(encoding_numerator)
        / f64::from(encoding_denominator)
        / 8_000.0;

    per_lane_gb_s * f64::from(width)
}

#[derive(Debug)]
struct ResolvedPciDevice {
    device_sysfs: PathBuf,
    canonical_sysfs: PathBuf,
    canonical_bdfs: Vec<(PciAddress, PathBuf)>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SlotAddress {
    domain: u16,
    bus: u8,
    device: u8,
    function: Option<u8>,
}

impl SlotAddress {
    fn matches(self, address: PciAddress) -> bool {
        self.domain == address.domain
            && self.bus == address.bus
            && self.device == address.device
            && self
                .function
                .is_none_or(|function| function == address.function)
    }
}

fn resolve_pci_device(
    sysfs_root: impl AsRef<Path>,
    address: PciAddress,
) -> Result<ResolvedPciDevice, PcieLinkUnknown> {
    let devices_dir = sysfs_root.as_ref().join(SYSFS_PCI_DEVICES);
    let path = devices_dir.join(address.sysfs_name());

    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(PcieLinkUnknown::MissingDevice(path));
        }
        Err(error) => {
            return Err(PcieLinkUnknown::UnreadableField {
                path,
                error: error.to_string(),
            });
        }
    };

    if !(metadata.is_dir() || metadata.file_type().is_symlink()) {
        return Err(PcieLinkUnknown::UnreliableDevicePath(path));
    }

    let Ok(canonical_root) = fs::canonicalize(sysfs_root.as_ref()) else {
        return Err(PcieLinkUnknown::UnreliableDevicePath(path));
    };

    let Ok(canonical_path) = fs::canonicalize(&path) else {
        return Err(PcieLinkUnknown::UnreliableDevicePath(path));
    };

    let Ok(target_metadata) = fs::metadata(&path) else {
        return Err(PcieLinkUnknown::UnreliableDevicePath(path));
    };

    if !target_metadata.is_dir() || !canonical_path.starts_with(canonical_root) {
        return Err(PcieLinkUnknown::UnreliableDevicePath(path));
    }

    let canonical_bdf_path = canonical_bdf_path(&canonical_path);
    let Some((canonical_address, _)) = canonical_bdf_path.last() else {
        return Err(PcieLinkUnknown::UnreliableDevicePath(path));
    };

    if *canonical_address != address {
        return Err(PcieLinkUnknown::UnreliableDevicePath(path));
    }

    Ok(ResolvedPciDevice {
        device_sysfs: path,
        canonical_sysfs: canonical_path,
        canonical_bdfs: canonical_bdf_path,
    })
}

fn negotiated_link_source_path(
    sysfs_root: &Path,
    device: &ResolvedPciDevice,
) -> Result<PathBuf, PcieLinkUnknown> {
    if device.canonical_bdfs.len() <= 1 {
        return Ok(device.canonical_sysfs.clone());
    }

    let matches = matching_slot_ancestor_paths(sysfs_root, device)?;
    if matches.len() == 1 {
        return Ok(matches[0].clone());
    }

    Err(PcieLinkUnknown::UnreliableDevicePath(
        device.device_sysfs.clone(),
    ))
}

fn matching_slot_ancestor_paths(
    sysfs_root: &Path,
    device: &ResolvedPciDevice,
) -> Result<Vec<PathBuf>, PcieLinkUnknown> {
    let slots_dir = sysfs_root.join(SYSFS_PCI_SLOTS);
    let entries = match fs::read_dir(&slots_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(PcieLinkUnknown::UnreadableField {
                path: slots_dir,
                error: error.to_string(),
            });
        }
    };

    let ancestors = &device.canonical_bdfs[..device.canonical_bdfs.len() - 1];
    let mut matches = Vec::new();

    for entry in entries {
        let entry = entry.map_err(|error| PcieLinkUnknown::UnreadableField {
            path: slots_dir.clone(),
            error: error.to_string(),
        })?;
        let address_path = entry.path().join("address");
        let address_text = match fs::read_to_string(&address_path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(PcieLinkUnknown::UnreadableField {
                    path: address_path,
                    error: error.to_string(),
                });
            }
        };

        let Some(slot_address) = parse_slot_address(&address_text) else {
            continue;
        };

        for (ancestor_address, ancestor_path) in ancestors {
            if slot_address.matches(*ancestor_address) {
                matches.push(ancestor_path.clone());
            }
        }
    }

    Ok(matches)
}

fn read_link_fields(path: &Path) -> Result<(LinkSpeed, u16), PcieLinkUnknown> {
    let speed_path = path.join("current_link_speed");
    let speed_text =
        fs::read_to_string(&speed_path).map_err(|error| PcieLinkUnknown::UnreadableField {
            path: speed_path,
            error: error.to_string(),
        })?;
    let Some(speed) = parse_link_speed(&speed_text) else {
        return Err(PcieLinkUnknown::UnrecognizedSpeed(
            speed_text.trim().to_owned(),
        ));
    };

    let width_path = path.join("current_link_width");
    let width_text =
        fs::read_to_string(&width_path).map_err(|error| PcieLinkUnknown::UnreadableField {
            path: width_path,
            error: error.to_string(),
        })?;
    let Some(width) = parse_link_width(&width_text) else {
        return Err(PcieLinkUnknown::UnrecognizedWidth(
            width_text.trim().to_owned(),
        ));
    };

    Ok((speed, width))
}

fn canonical_bdf_path(canonical_path: &Path) -> Vec<(PciAddress, PathBuf)> {
    let mut current = PathBuf::new();
    let mut bdf_path = Vec::new();

    for component in canonical_path.components() {
        current.push(component.as_os_str());
        let Some(name) = component.as_os_str().to_str() else {
            continue;
        };
        let Some(address) = parse_pci_address_component(name) else {
            continue;
        };

        bdf_path.push((address, current.clone()));
    }

    bdf_path
}

fn parse_pci_address_component(text: &str) -> Option<PciAddress> {
    let address = parse_slot_address(text)?;
    let function = address.function?;

    Some(PciAddress {
        domain: address.domain,
        bus: address.bus,
        device: address.device,
        function,
    })
}

fn parse_slot_address(text: &str) -> Option<SlotAddress> {
    let mut parts = text.trim().split(':');
    let domain = parts.next()?;
    let bus = parts.next()?;
    let device_function = parts.next()?;

    if parts.next().is_some() {
        return None;
    }

    let (device, function) = match device_function.split_once('.') {
        Some((device, function)) => (device, Some(function)),
        None => (device_function, None),
    };

    let domain = parse_hex_u16(domain, 0xffff)?;
    let bus = parse_hex_u8(bus, 0xff)?;
    let device = parse_hex_u8(device, 0x1f)?;
    let function = match function {
        Some(function) => Some(parse_hex_u8(function, 0x7)?),
        None => None,
    };

    Some(SlotAddress {
        domain,
        bus,
        device,
        function,
    })
}

fn parse_hex_u16(text: &str, max: u16) -> Option<u16> {
    if text.is_empty() || text.len() > 4 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }

    let value = u16::from_str_radix(text, 16).ok()?;
    if value <= max { Some(value) } else { None }
}

fn parse_hex_u8(text: &str, max: u8) -> Option<u8> {
    if text.is_empty() || text.len() > 2 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }

    let value = u8::from_str_radix(text, 16).ok()?;
    if value <= max { Some(value) } else { None }
}

fn generation_from_gt_per_second_milli(gt_per_second_milli: u32) -> Option<PcieGeneration> {
    match gt_per_second_milli {
        2_500 => Some(PcieGeneration::Gen1),
        5_000 => Some(PcieGeneration::Gen2),
        8_000 => Some(PcieGeneration::Gen3),
        16_000 => Some(PcieGeneration::Gen4),
        32_000 => Some(PcieGeneration::Gen5),
        64_000 => Some(PcieGeneration::Gen6),
        128_000 => Some(PcieGeneration::Gen7),
        _ => None,
    }
}

fn parse_decimal_milli(text: &str) -> Option<u32> {
    let (whole, fraction) = match text.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (text, None),
    };

    if whole.is_empty() || !whole.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }

    let whole = whole.parse::<u32>().ok()?;
    let fraction = match fraction {
        Some(fraction) => {
            if fraction.is_empty()
                || fraction.len() > 3
                || !fraction.bytes().all(|byte| byte.is_ascii_digit())
            {
                return None;
            }

            let mut padded = fraction.to_owned();
            while padded.len() < 3 {
                padded.push('0');
            }
            padded.parse::<u32>().ok()?
        }
        None => 0,
    };

    whole.checked_mul(1_000)?.checked_add(fraction)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestSysfs {
        root: PathBuf,
    }

    impl TestSysfs {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock before Unix epoch")
                .as_nanos();
            let root =
                std::env::temp_dir().join(format!("xfer-pcie-test-{}-{nonce}", std::process::id()));
            fs::create_dir_all(root.join(SYSFS_PCI_DEVICES)).expect("create fake sysfs");
            Self { root }
        }

        fn device_dir(&self, address: PciAddress) -> PathBuf {
            self.root.join(SYSFS_PCI_DEVICES).join(address.sysfs_name())
        }

        fn add_device(&self, address: PciAddress, speed: &str, width: &str) {
            let device = self.device_dir(address);
            fs::create_dir_all(&device).expect("create fake PCI device");
            write_link(&device, speed, width);
        }

        fn slot_dir(&self, slot: &str) -> PathBuf {
            self.root.join(SYSFS_PCI_SLOTS).join(slot)
        }

        fn add_slot(&self, slot: &str, address: &str) {
            let slot_dir = self.slot_dir(slot);
            fs::create_dir_all(&slot_dir).expect("create fake PCI slot");
            write_file(&slot_dir.join("address"), address);
        }

        #[cfg(unix)]
        fn add_nested_device(
            &self,
            endpoint: PciAddress,
            chain: &[PciAddress],
            slot_speed: &str,
            slot_width: &str,
            endpoint_speed: &str,
            endpoint_width: &str,
        ) -> PathBuf {
            use std::os::unix::fs::symlink;

            assert_eq!(chain.last(), Some(&endpoint));

            let mut current = self
                .root
                .join("devices")
                .join(format!("pci{:04x}:{:02x}", chain[0].domain, chain[0].bus));
            fs::create_dir_all(&current).expect("create fake PCI domain root");

            let mut slot_path = None;
            for (index, address) in chain.iter().enumerate() {
                current = current.join(address.sysfs_name());
                fs::create_dir_all(&current).expect("create fake canonical PCI path");
                if index == 0 {
                    write_link(&current, slot_speed, slot_width);
                    slot_path = Some(current.clone());
                }
            }

            write_link(&current, endpoint_speed, endpoint_width);
            symlink(&current, self.device_dir(endpoint)).expect("create fake bus PCI symlink");

            slot_path.expect("chain contains a slot/root-port address")
        }
    }

    impl Drop for TestSysfs {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn write_file(path: &Path, contents: &str) {
        let mut file = File::create(path).expect("create test file");
        file.write_all(contents.as_bytes())
            .expect("write test file");
    }

    fn write_link(path: &Path, speed: &str, width: &str) {
        write_file(&path.join("current_link_speed"), speed);
        write_file(&path.join("current_link_width"), width);
    }

    fn assert_close(actual: f64, expected: f64) {
        let difference = (actual - expected).abs();
        assert!(
            difference < 0.000_001,
            "actual {actual}, expected {expected}, difference {difference}"
        );
    }

    #[test]
    fn constructs_canonical_sysfs_bdf_names() {
        let address = PciAddress::new(0, 3, 0, 1).expect("valid address");

        assert_eq!(address.sysfs_name(), "0000:03:00.1");
        assert_eq!(address.to_string(), "0000:03:00.1");
    }

    #[test]
    fn rejects_invalid_level_zero_pci_addresses() {
        assert_eq!(PciAddress::new(0x1_0000, 0, 0, 0), None);
        assert_eq!(PciAddress::new(0, 0x100, 0, 0), None);
        assert_eq!(PciAddress::new(0, 0, 0x20, 0), None);
        assert_eq!(PciAddress::new(0, 0, 0, 0x8), None);
    }

    #[test]
    fn parses_known_link_speeds_and_generations() {
        let cases = [
            ("2.5 GT/s PCIe\n", PcieGeneration::Gen1, 2_500),
            ("5.0 GT/s PCIe", PcieGeneration::Gen2, 5_000),
            ("8.0 GT/s PCIe", PcieGeneration::Gen3, 8_000),
            ("16.0 GT/s PCIe", PcieGeneration::Gen4, 16_000),
            ("32.0 GT/s PCIe", PcieGeneration::Gen5, 32_000),
            ("64.0 GT/s PCIe", PcieGeneration::Gen6, 64_000),
            ("128.0 GT/s PCIe", PcieGeneration::Gen7, 128_000),
            ("8 GT/s", PcieGeneration::Gen3, 8_000),
        ];

        for (text, generation, gt_per_second_milli) in cases {
            assert_eq!(
                parse_link_speed(text),
                Some(LinkSpeed {
                    generation,
                    gt_per_second_milli,
                })
            );
        }
    }

    #[test]
    fn rejects_malformed_or_unrecognized_link_speeds() {
        assert_eq!(parse_link_speed(""), None);
        assert_eq!(parse_link_speed("16.0"), None);
        assert_eq!(parse_link_speed("16.0 Gb/s PCIe"), None);
        assert_eq!(parse_link_speed("12.0 GT/s PCIe"), None);
        assert_eq!(parse_link_speed("-16.0 GT/s PCIe"), None);
        assert_eq!(parse_link_speed("16.0000 GT/s PCIe"), None);
    }

    #[test]
    fn parses_link_widths() {
        assert_eq!(parse_link_width("16\n"), Some(16));
        assert_eq!(parse_link_width("x8"), Some(8));
        assert_eq!(parse_link_width("X4"), Some(4));
    }

    #[test]
    fn rejects_invalid_link_widths() {
        assert_eq!(parse_link_width(""), None);
        assert_eq!(parse_link_width("0"), None);
        assert_eq!(parse_link_width("x0"), None);
        assert_eq!(parse_link_width("65"), None);
        assert_eq!(parse_link_width("x16 lanes"), None);
    }

    #[test]
    fn computes_theoretical_payload_bandwidth_using_pcie_encoding() {
        assert_close(theoretical_payload_gb_s(PcieGeneration::Gen1, 16), 4.0);
        assert_close(theoretical_payload_gb_s(PcieGeneration::Gen2, 16), 8.0);
        assert_close(
            theoretical_payload_gb_s(PcieGeneration::Gen3, 16),
            15.753_846_153_846_153,
        );
        assert_close(
            theoretical_payload_gb_s(PcieGeneration::Gen4, 16),
            31.507_692_307_692_307,
        );
        assert_close(
            theoretical_payload_gb_s(PcieGeneration::Gen5, 16),
            63.015_384_615_384_61,
        );
        assert_close(theoretical_payload_gb_s(PcieGeneration::Gen6, 16), 121.0);
        assert_close(theoretical_payload_gb_s(PcieGeneration::Gen7, 16), 242.0);
        assert_close(theoretical_payload_gb_s(PcieGeneration::Gen5, 0), 0.0);
    }

    #[test]
    fn reads_known_link_from_injected_sysfs_root() {
        let sysfs = TestSysfs::new();
        let address = PciAddress::new(0, 3, 0, 0).expect("valid address");
        sysfs.add_device(address, "32.0 GT/s PCIe\n", "16\n");

        let status = read_link_from_sysfs(&sysfs.root, address);

        match status {
            PcieLinkStatus::Known(link) => {
                assert_eq!(link.address, address);
                assert_eq!(link.sysfs_path, sysfs.device_dir(address));
                assert_eq!(link.speed.generation, PcieGeneration::Gen5);
                assert_eq!(link.speed.gt_per_second_milli, 32_000);
                assert_eq!(link.width, 16);
                assert_close(link.theoretical_gb_s, 63.015_384_615_384_61);
            }
            PcieLinkStatus::Unknown(reason) => panic!("expected known link, got {reason:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn reads_nested_gpu_link_from_unique_physical_slot_ancestor() {
        let sysfs = TestSysfs::new();
        let slot = PciAddress::new(0, 0x16, 0, 0).expect("valid slot address");
        let bridge = PciAddress::new(0, 0x17, 0, 0).expect("valid bridge address");
        let downstream = PciAddress::new(0, 0x18, 1, 0).expect("valid downstream address");
        let endpoint = PciAddress::new(0, 0x19, 0, 0).expect("valid endpoint address");
        let slot_path = sysfs.add_nested_device(
            endpoint,
            &[slot, bridge, downstream, endpoint],
            "8.0 GT/s PCIe\n",
            "16\n",
            "2.5 GT/s PCIe\n",
            "1\n",
        );
        sysfs.add_slot("4", "0000:16:00\n");

        let status = read_link_from_sysfs(&sysfs.root, endpoint);

        match status {
            PcieLinkStatus::Known(link) => {
                assert_eq!(link.address, endpoint);
                assert_eq!(link.sysfs_path, slot_path);
                assert_eq!(link.speed.generation, PcieGeneration::Gen3);
                assert_eq!(link.speed.gt_per_second_milli, 8_000);
                assert_eq!(link.width, 16);
                assert_close(link.theoretical_gb_s, 15.753_846_153_846_153);
            }
            PcieLinkStatus::Unknown(reason) => panic!("expected known link, got {reason:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn returns_unknown_for_nested_gpu_without_slot_mapping() {
        let sysfs = TestSysfs::new();
        let slot = PciAddress::new(0, 0x16, 0, 0).expect("valid slot address");
        let bridge = PciAddress::new(0, 0x17, 0, 0).expect("valid bridge address");
        let endpoint = PciAddress::new(0, 0x19, 0, 0).expect("valid endpoint address");
        sysfs.add_nested_device(
            endpoint,
            &[slot, bridge, endpoint],
            "8.0 GT/s PCIe\n",
            "16\n",
            "2.5 GT/s PCIe\n",
            "1\n",
        );

        let status = read_link_from_sysfs(&sysfs.root, endpoint);

        assert_eq!(
            status,
            PcieLinkStatus::Unknown(PcieLinkUnknown::UnreliableDevicePath(
                sysfs.device_dir(endpoint)
            ))
        );
    }

    #[cfg(unix)]
    #[test]
    fn returns_unknown_for_nested_gpu_with_ambiguous_slot_mapping() {
        let sysfs = TestSysfs::new();
        let slot = PciAddress::new(0, 0x16, 0, 0).expect("valid slot address");
        let bridge = PciAddress::new(0, 0x17, 0, 0).expect("valid bridge address");
        let endpoint = PciAddress::new(0, 0x19, 0, 0).expect("valid endpoint address");
        sysfs.add_nested_device(
            endpoint,
            &[slot, bridge, endpoint],
            "8.0 GT/s PCIe\n",
            "16\n",
            "2.5 GT/s PCIe\n",
            "1\n",
        );
        sysfs.add_slot("4", "0000:16:00\n");
        sysfs.add_slot("5", "0000:17:00.0\n");

        let status = read_link_from_sysfs(&sysfs.root, endpoint);

        assert_eq!(
            status,
            PcieLinkStatus::Unknown(PcieLinkUnknown::UnreliableDevicePath(
                sysfs.device_dir(endpoint)
            ))
        );
    }

    #[test]
    fn returns_unknown_when_bdf_mapping_is_missing() {
        let sysfs = TestSysfs::new();
        let address = PciAddress::new(0, 3, 0, 0).expect("valid address");

        let status = read_link_from_sysfs(&sysfs.root, address);

        assert_eq!(
            status,
            PcieLinkStatus::Unknown(PcieLinkUnknown::MissingDevice(sysfs.device_dir(address)))
        );
    }

    #[test]
    fn returns_unknown_for_invalid_level_zero_address() {
        let sysfs = TestSysfs::new();

        let status = read_link_for_level_zero_pci_from_sysfs(&sysfs.root, 0, 0, 0x20, 0);

        assert_eq!(
            status,
            PcieLinkStatus::Unknown(PcieLinkUnknown::InvalidAddress)
        );
    }

    #[test]
    fn returns_unknown_when_device_path_is_not_a_directory_or_symlink() {
        let sysfs = TestSysfs::new();
        let address = PciAddress::new(0, 3, 0, 0).expect("valid address");
        let device = sysfs.device_dir(address);
        write_file(&device, "");

        let status = read_link_from_sysfs(&sysfs.root, address);

        assert_eq!(
            status,
            PcieLinkStatus::Unknown(PcieLinkUnknown::UnreliableDevicePath(device))
        );
    }

    #[cfg(unix)]
    #[test]
    fn returns_unknown_when_device_symlink_escapes_sysfs_root() {
        use std::os::unix::fs::symlink;

        let sysfs = TestSysfs::new();
        let escaped = TestSysfs::new();
        let address = PciAddress::new(0, 3, 0, 0).expect("valid address");
        escaped.add_device(address, "32.0 GT/s PCIe\n", "16\n");
        let device = sysfs.device_dir(address);
        symlink(escaped.device_dir(address), &device).expect("create escaping symlink");

        let status = read_link_from_sysfs(&sysfs.root, address);

        assert_eq!(
            status,
            PcieLinkStatus::Unknown(PcieLinkUnknown::UnreliableDevicePath(device))
        );
    }

    #[cfg(unix)]
    #[test]
    fn returns_unknown_when_canonical_path_has_wrong_endpoint_identity() {
        use std::os::unix::fs::symlink;

        let sysfs = TestSysfs::new();
        let requested = PciAddress::new(0, 3, 0, 0).expect("valid requested address");
        let other = PciAddress::new(0, 4, 0, 0).expect("valid other address");
        let other_path = sysfs
            .root
            .join("devices")
            .join("pci0000:04")
            .join(other.sysfs_name());
        fs::create_dir_all(&other_path).expect("create wrong canonical endpoint");
        write_link(&other_path, "32.0 GT/s PCIe\n", "16\n");
        let device = sysfs.device_dir(requested);
        symlink(other_path, &device).expect("create wrong-endpoint symlink");

        let status = read_link_from_sysfs(&sysfs.root, requested);

        assert_eq!(
            status,
            PcieLinkStatus::Unknown(PcieLinkUnknown::UnreliableDevicePath(device))
        );
    }

    #[test]
    fn returns_unknown_for_unrecognized_speed_or_width() {
        let sysfs = TestSysfs::new();
        let address = PciAddress::new(0, 3, 0, 0).expect("valid address");
        sysfs.add_device(address, "12.0 GT/s PCIe\n", "16\n");

        assert_eq!(
            read_link_from_sysfs(&sysfs.root, address),
            PcieLinkStatus::Unknown(PcieLinkUnknown::UnrecognizedSpeed(
                "12.0 GT/s PCIe".to_owned()
            ))
        );

        sysfs.add_device(address, "16.0 GT/s PCIe\n", "0\n");

        assert_eq!(
            read_link_from_sysfs(&sysfs.root, address),
            PcieLinkStatus::Unknown(PcieLinkUnknown::UnrecognizedWidth("0".to_owned()))
        );
    }
}
