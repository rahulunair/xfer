use std::fmt;
use std::fs;
use std::path::Path;

use super::error::{EvidenceError, Result, read_error};
use super::intel_perfmon::PerfmonProfileId;

// CPU model names and values are verified against the GPL-2.0 Linux v7.0
// arch/x86/include/asm/intel-family.h source at commit
// 028ef9c96e96197026887c0f092424679298aae8
// (file SHA-256 d9c332d0be172cb9c45c8051f4fb52e0f55ef406c49e43520e6ef085d4e078fc).
// No Linux GPL source is vendored.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CpuVendor {
    GenuineIntel,
    Other,
}

impl fmt::Display for CpuVendor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GenuineIntel => f.write_str("GenuineIntel"),
            Self::Other => f.write_str("other"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CpuModel(u16);

impl CpuModel {
    pub const SAPPHIRE_RAPIDS_X: Self = Self(0x8f);
    pub const GRANITE_RAPIDS_X: Self = Self(0xad);
    pub const GRANITE_RAPIDS_D: Self = Self(0xae);

    pub fn new(model: u16) -> Self {
        Self(model)
    }

    pub fn value(self) -> u16 {
        self.0
    }
}

impl fmt::Display for CpuModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{:02x}", self.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CpuIdentity {
    vendor: CpuVendor,
    family: u16,
    model: CpuModel,
}

impl CpuIdentity {
    pub fn new(vendor: CpuVendor, family: u16, model: CpuModel) -> Self {
        Self {
            vendor,
            family,
            model,
        }
    }

    pub fn vendor(self) -> CpuVendor {
        self.vendor
    }

    pub fn family(self) -> u16 {
        self.family
    }

    pub fn model(self) -> CpuModel {
        self.model
    }

    pub fn matching_profile(self) -> Result<CpuProfile> {
        CpuProfile::for_cpu(self).ok_or(EvidenceError::UnsupportedCpu(self))
    }
}

impl fmt::Display for CpuIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} family {} model {}",
            self.vendor, self.family, self.model
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CpuProfile {
    SapphireRapidsX,
    GraniteRapidsX,
    GraniteRapidsD,
}

impl CpuProfile {
    pub fn for_cpu(cpu: CpuIdentity) -> Option<Self> {
        if cpu.vendor != CpuVendor::GenuineIntel || cpu.family != 6 {
            return None;
        }

        match cpu.model {
            CpuModel::SAPPHIRE_RAPIDS_X => Some(Self::SapphireRapidsX),
            CpuModel::GRANITE_RAPIDS_X => Some(Self::GraniteRapidsX),
            CpuModel::GRANITE_RAPIDS_D => Some(Self::GraniteRapidsD),
            _ => None,
        }
    }

    pub fn perfmon_profile(self) -> PerfmonProfileId {
        match self {
            Self::SapphireRapidsX => PerfmonProfileId::SapphireRapids,
            Self::GraniteRapidsX | Self::GraniteRapidsD => PerfmonProfileId::GraniteRapids,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::SapphireRapidsX => "Intel Sapphire Rapids-X",
            Self::GraniteRapidsX => "Intel Granite Rapids-X",
            Self::GraniteRapidsD => "Intel Granite Rapids-D",
        }
    }
}

pub fn read_cpu_identity() -> Result<CpuIdentity> {
    read_cpu_identity_from_path(Path::new("/proc/cpuinfo"))
}

pub fn read_cpu_identity_from_path(path: impl AsRef<Path>) -> Result<CpuIdentity> {
    let path = path.as_ref();
    let text = fs::read_to_string(path).map_err(|error| read_error(path, error))?;
    parse_cpuinfo(&text).map_err(|reason| EvidenceError::MalformedSysfs {
        path: path.to_path_buf(),
        reason,
    })
}

pub fn parse_cpuinfo(text: &str) -> std::result::Result<CpuIdentity, String> {
    let records = parse_cpuinfo_records(text)?;
    let mut identities = records.into_iter().map(CpuInfoRecord::identity);
    let Some(first) = identities.next() else {
        return Err("no complete processor records in /proc/cpuinfo".to_owned());
    };

    for identity in identities {
        if identity != first {
            return Err(format!(
                "mixed CPU identities in /proc/cpuinfo: {first} and {identity}"
            ));
        }
    }

    Ok(first)
}

fn parse_cpuinfo_records(text: &str) -> std::result::Result<Vec<CpuInfoRecord>, String> {
    let mut records = Vec::new();
    let mut current = CpuInfoRecordBuilder::default();

    for line in text.lines() {
        if line.trim().is_empty() {
            if current.has_any_field() {
                records.push(current.build()?);
                current = CpuInfoRecordBuilder::default();
            }
            continue;
        }

        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        current.accept(key.trim(), value.trim())?;
    }

    if current.has_any_field() {
        records.push(current.build()?);
    }

    Ok(records)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CpuInfoRecord {
    vendor: CpuVendor,
    family: u16,
    model: CpuModel,
}

impl CpuInfoRecord {
    fn identity(self) -> CpuIdentity {
        CpuIdentity::new(self.vendor, self.family, self.model)
    }
}

#[derive(Default)]
struct CpuInfoRecordBuilder {
    vendor: Option<CpuVendor>,
    family: Option<u16>,
    model: Option<CpuModel>,
}

impl CpuInfoRecordBuilder {
    fn has_any_field(&self) -> bool {
        self.vendor.is_some() || self.family.is_some() || self.model.is_some()
    }

    fn accept(&mut self, key: &str, value: &str) -> std::result::Result<(), String> {
        match key {
            "vendor_id" => {
                self.vendor = Some(if value == "GenuineIntel" {
                    CpuVendor::GenuineIntel
                } else {
                    CpuVendor::Other
                });
            }
            "cpu family" => {
                self.family = Some(
                    value
                        .parse::<u16>()
                        .map_err(|error| format!("invalid cpu family '{value}': {error}"))?,
                );
            }
            "model" => {
                self.model =
                    Some(CpuModel::new(value.parse::<u16>().map_err(|error| {
                        format!("invalid model '{value}': {error}")
                    })?));
            }
            _ => {}
        }
        Ok(())
    }

    fn build(self) -> std::result::Result<CpuInfoRecord, String> {
        let Some(vendor) = self.vendor else {
            return Err("processor record missing vendor_id".to_owned());
        };
        let Some(family) = self.family else {
            return Err("processor record missing cpu family".to_owned());
        };
        let Some(model) = self.model else {
            return Err("processor record missing model".to_owned());
        };
        Ok(CpuInfoRecord {
            vendor,
            family,
            model,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_supported_intel_server_profiles() {
        let spr = CpuIdentity::new(CpuVendor::GenuineIntel, 6, CpuModel::SAPPHIRE_RAPIDS_X);
        let gnr = CpuIdentity::new(CpuVendor::GenuineIntel, 6, CpuModel::GRANITE_RAPIDS_X);
        let gnr_d = CpuIdentity::new(CpuVendor::GenuineIntel, 6, CpuModel::GRANITE_RAPIDS_D);

        assert_eq!(
            spr.matching_profile().expect("SPR-X profile"),
            CpuProfile::SapphireRapidsX
        );
        assert_eq!(
            gnr.matching_profile().expect("GNR-X profile"),
            CpuProfile::GraniteRapidsX
        );
        assert_eq!(
            gnr_d.matching_profile().expect("GNR-D profile"),
            CpuProfile::GraniteRapidsD
        );
        assert_eq!(
            gnr_d
                .matching_profile()
                .expect("GNR-D profile")
                .perfmon_profile(),
            PerfmonProfileId::GraniteRapids
        );
        assert_eq!(CpuProfile::GraniteRapidsD.name(), "Intel Granite Rapids-D");
    }

    #[test]
    fn rejects_unsupported_cpu() {
        let cpu = CpuIdentity::new(CpuVendor::GenuineIntel, 6, CpuModel::new(0x55));
        assert!(matches!(
            cpu.matching_profile(),
            Err(EvidenceError::UnsupportedCpu(_))
        ));

        let cpu = CpuIdentity::new(CpuVendor::Other, 6, CpuModel::SAPPHIRE_RAPIDS_X);
        assert!(matches!(
            cpu.matching_profile(),
            Err(EvidenceError::UnsupportedCpu(_))
        ));
    }

    #[test]
    fn parses_complete_consistent_cpuinfo_records() {
        let cpuinfo = "\
processor : 0
vendor_id : GenuineIntel
cpu family : 6
model : 143

processor : 1
vendor_id : GenuineIntel
cpu family : 6
model : 143
";

        let cpu = parse_cpuinfo(cpuinfo).expect("cpuinfo");
        assert_eq!(
            cpu,
            CpuIdentity::new(CpuVendor::GenuineIntel, 6, CpuModel::SAPPHIRE_RAPIDS_X)
        );
    }

    #[test]
    fn rejects_mixed_cpuinfo_identities() {
        let cpuinfo = "\
processor : 0
vendor_id : GenuineIntel
cpu family : 6
model : 143

processor : 1
vendor_id : GenuineIntel
cpu family : 6
model : 173
";

        assert!(parse_cpuinfo(cpuinfo).is_err());
    }

    #[test]
    fn rejects_incomplete_cpuinfo_records_without_combining_fields() {
        let cpuinfo = "\
processor : 0
vendor_id : GenuineIntel
cpu family : 6

processor : 1
model : 143
";

        assert!(parse_cpuinfo(cpuinfo).is_err());
    }
}
