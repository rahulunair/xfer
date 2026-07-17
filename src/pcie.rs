use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

const SYSFS_PCI_DEVICES: &str = "bus/pci/devices";

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
    let device_path = match pci_device_path(sysfs_root.as_ref(), address) {
        Ok(path) => path,
        Err(reason) => return PcieLinkStatus::Unknown(reason),
    };

    let speed_path = device_path.join("current_link_speed");
    let speed_text = match read_sysfs_field(&speed_path) {
        Ok(text) => text,
        Err(error) => {
            return PcieLinkStatus::Unknown(PcieLinkUnknown::UnreadableField {
                path: speed_path,
                error,
            });
        }
    };

    let Some(speed) = parse_link_speed(&speed_text) else {
        return PcieLinkStatus::Unknown(PcieLinkUnknown::UnrecognizedSpeed(
            speed_text.trim().to_owned(),
        ));
    };

    let width_path = device_path.join("current_link_width");
    let width_text = match read_sysfs_field(&width_path) {
        Ok(text) => text,
        Err(error) => {
            return PcieLinkStatus::Unknown(PcieLinkUnknown::UnreadableField {
                path: width_path,
                error,
            });
        }
    };

    let Some(width) = parse_link_width(&width_text) else {
        return PcieLinkStatus::Unknown(PcieLinkUnknown::UnrecognizedWidth(
            width_text.trim().to_owned(),
        ));
    };

    PcieLinkStatus::Known(PcieLink {
        address,
        sysfs_path: device_path,
        speed,
        width,
        theoretical_gb_s: theoretical_payload_gb_s(speed.generation, width),
    })
}

pub fn pci_device_path(
    sysfs_root: impl AsRef<Path>,
    address: PciAddress,
) -> Result<PathBuf, PcieLinkUnknown> {
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

    Ok(path)
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

fn read_sysfs_field(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|error| error.to_string())
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
            let root = std::env::temp_dir().join(format!(
                "xferbench-pcie-test-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir_all(root.join(SYSFS_PCI_DEVICES)).expect("create fake sysfs");
            Self { root }
        }

        fn device_dir(&self, address: PciAddress) -> PathBuf {
            self.root.join(SYSFS_PCI_DEVICES).join(address.sysfs_name())
        }

        fn add_device(&self, address: PciAddress, speed: &str, width: &str) {
            let device = self.device_dir(address);
            fs::create_dir_all(&device).expect("create fake PCI device");
            write_file(&device.join("current_link_speed"), speed);
            write_file(&device.join("current_link_width"), width);
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
