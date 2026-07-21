use std::fmt;

use super::error::{EvidenceError, Result};
use super::linux_pmu::{FormatFieldName, PackedConfig, PmuFieldValue, PmuFormat};

mod generated;

pub use generated::{ATTRIBUTION, MAPFILE_ROWS, PROFILES, SOURCES, UPSTREAM_COMMIT};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PerfmonProfileId {
    SapphireRapids,
    GraniteRapids,
}

impl fmt::Display for PerfmonProfileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SapphireRapids => f.write_str("SPR"),
            Self::GraniteRapids => f.write_str("GNR"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PerfmonUnit {
    Iio,
    UpiLl,
}

impl fmt::Display for PerfmonUnit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Iio => f.write_str("IIO"),
            Self::UpiLl => f.write_str("UPI LL"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventRole {
    IioDataReqOfCpuMemReadAllParts,
    IioDataReqOfCpuMemWriteAllParts,
    IioDataReqOfCpuPeerWriteAllParts,
    IioDataReqOfCpuPeerReadAllParts,
    IioDataReqByCpuPeerWriteAllParts,
    IioDataReqByCpuPeerReadAllParts,
    UpiTxDataFlitsAll,
    UpiRxDataFlitsAll,
}

impl fmt::Display for EventRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IioDataReqOfCpuMemReadAllParts => {
                f.write_str("IIO data requested of CPU memory read all parts")
            }
            Self::IioDataReqOfCpuMemWriteAllParts => {
                f.write_str("IIO data requested of CPU memory write all parts")
            }
            Self::IioDataReqOfCpuPeerWriteAllParts => {
                f.write_str("IIO OF_CPU peer write all parts")
            }
            Self::IioDataReqOfCpuPeerReadAllParts => f.write_str("IIO OF_CPU peer read all parts"),
            Self::IioDataReqByCpuPeerWriteAllParts => {
                f.write_str("IIO BY_CPU peer write all parts")
            }
            Self::IioDataReqByCpuPeerReadAllParts => f.write_str("IIO BY_CPU peer read all parts"),
            Self::UpiTxDataFlitsAll => f.write_str("UPI transmitted data flits"),
            Self::UpiRxDataFlitsAll => f.write_str("UPI received data flits"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PerfmonSourceKind {
    Uncore,
    UncoreExperimental,
}

impl fmt::Display for PerfmonSourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Uncore => f.write_str("uncore"),
            Self::UncoreExperimental => f.write_str("uncore experimental"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeField {
    EventCode,
    UMask,
    PortMask,
    FCMask,
}

impl RuntimeField {
    pub fn source_name(self) -> &'static str {
        match self {
            Self::EventCode => "EventCode",
            Self::UMask => "UMask",
            Self::PortMask => "PortMask",
            Self::FCMask => "FCMask",
        }
    }

    fn linux_format_field(self) -> FormatFieldName {
        match self {
            Self::EventCode => FormatFieldName::event(),
            Self::UMask => FormatFieldName::umask(),
            Self::PortMask => FormatFieldName::ch_mask(),
            Self::FCMask => FormatFieldName::fc_mask(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeFieldValue {
    pub field: RuntimeField,
    pub value: u64,
    pub raw: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceField {
    pub name: &'static str,
    pub value: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProgrammableCounter(u8);

impl ProgrammableCounter {
    pub const fn new(value: u8) -> Self {
        Self(value)
    }

    pub const fn value(self) -> u8 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CounterRestriction {
    pub raw: &'static str,
    pub counters: &'static [ProgrammableCounter],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventProvenance {
    Direct {
        source_path: &'static str,
        source_event_name: &'static str,
        source_record_sha256: &'static str,
    },
    DerivedUnion {
        source_path: &'static str,
        source_event_names: &'static [&'static str],
        source_record_sha256: &'static [&'static str],
        rule: &'static str,
    },
}

impl EventProvenance {
    pub fn source_path(self) -> &'static str {
        match self {
            Self::Direct { source_path, .. } | Self::DerivedUnion { source_path, .. } => {
                source_path
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PerfmonAttribution {
    pub upstream_repository: &'static str,
    pub upstream_commit: &'static str,
    pub license: &'static str,
    pub license_path: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PerfmonEvent {
    pub role: EventRole,
    pub unit: PerfmonUnit,
    pub brief_description: &'static str,
    pub public_description: &'static str,
    pub counter: CounterRestriction,
    pub runtime_fields: &'static [RuntimeFieldValue],
    pub source_fields: &'static [SourceField],
    pub provenance: EventProvenance,
}

impl PerfmonEvent {
    pub fn encode_for_linux(self, format: &PmuFormat) -> Result<PackedConfig> {
        validate_runtime_fields(self.runtime_fields)?;
        let values = self
            .runtime_fields
            .iter()
            .map(|field| PmuFieldValue::new(field.field.linux_format_field(), field.value))
            .collect::<Vec<_>>();
        format.pack(&values)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PerfmonSource {
    pub path: &'static str,
    pub kind: PerfmonSourceKind,
    pub copyright: &'static str,
    pub info: &'static str,
    pub event_db_version: &'static str,
    pub date_published: &'static str,
    pub upstream_sha256: &'static str,
    pub selected_event_names: &'static [&'static str],
}

fn validate_runtime_fields(fields: &[RuntimeFieldValue]) -> Result<()> {
    const REQUIRED: [RuntimeField; 4] = [
        RuntimeField::EventCode,
        RuntimeField::UMask,
        RuntimeField::PortMask,
        RuntimeField::FCMask,
    ];

    for required in REQUIRED {
        let count = fields
            .iter()
            .filter(|field| field.field == required)
            .count();
        if count != 1 {
            return Err(EvidenceError::InvalidEventEncoding {
                reason: format!(
                    "runtime field {} must appear exactly once, found {count}",
                    required.source_name()
                ),
            });
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MapfileRow {
    pub family_model: &'static str,
    pub version: &'static str,
    pub filename: &'static str,
    pub event_type: PerfmonSourceKind,
    pub source_line: u32,
    pub source_row_sha256: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PerfmonProfile {
    pub id: PerfmonProfileId,
    pub cpu_models: &'static [u16],
    pub mapfile_rows: &'static [MapfileRow],
    pub events: &'static [PerfmonEvent],
}

impl PerfmonProfile {
    pub fn event(self, role: EventRole) -> Result<&'static PerfmonEvent> {
        self.events
            .iter()
            .find(|event| event.role == role)
            .ok_or_else(|| EvidenceError::UnavailableEvent {
                profile: self.id,
                role,
                reason: "profile does not contain this event role".to_owned(),
            })
    }
}

pub fn profile(id: PerfmonProfileId) -> &'static PerfmonProfile {
    PROFILES
        .iter()
        .find(|profile| profile.id == id)
        .expect("generated perfmon profiles contain every PerfmonProfileId")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::super::linux_pmu::{PmuKind, discover_pmus_from_sysfs};
    use super::*;

    #[test]
    fn preserves_upstream_commit_and_versions() {
        assert_eq!(UPSTREAM_COMMIT, "6e3329d20457aad11d8cc323b85aa6a16b075918");
        assert!(SOURCES.iter().any(|source| {
            source.path == "SPR/events/sapphirerapids_uncore.json"
                && source.event_db_version == "1.39"
                && source.upstream_sha256
                    == "51eaed4092290ef9275a5b10a4ce9412b319cb19889e7c9ae9853f8b672e8a37"
        }));
        assert!(SOURCES.iter().any(|source| {
            source.path == "GNR/events/graniterapids_uncore_experimental.json"
                && source.event_db_version == "1.20"
                && source.upstream_sha256
                    == "3d8500e8a5a7a8426c79e92f279219119099f288d425545d0f895144285df14f"
        }));
    }

    #[test]
    fn preserves_gnr_peer_all_parts_as_direct_source_record() {
        let event = profile(PerfmonProfileId::GraniteRapids)
            .event(EventRole::IioDataReqOfCpuPeerWriteAllParts)
            .expect("GNR peer write all parts");

        assert_eq!(event.counter.raw, "0,1");
        assert!(matches!(
            event.provenance,
            EventProvenance::Direct {
                source_path: "GNR/events/graniterapids_uncore_experimental.json",
                source_event_name: "UNC_IIO_DATA_REQ_OF_CPU.PEER_WRITE.ALL_PARTS",
                ..
            }
        ));
        assert!(
            event
                .source_fields
                .iter()
                .any(|field| field.name == "UMaskExt" && field.value == "0x00070FF0")
        );
    }

    #[test]
    fn gnr_umask_ext_is_source_metadata_not_linux_runtime_encoding() {
        let event = profile(PerfmonProfileId::GraniteRapids)
            .event(EventRole::IioDataReqOfCpuPeerWriteAllParts)
            .expect("GNR peer write all parts");
        let temp = TempSysfs::new();
        let iio0 = temp.pmu("uncore_iio_0");
        write_file(iio0.join("type"), "67\n");
        write_file(iio0.join("format/event"), "config:0-7\n");
        write_file(iio0.join("format/umask"), "config:8-15\n");
        write_file(iio0.join("format/ch_mask"), "config:36-47\n");
        write_file(iio0.join("format/fc_mask"), "config:48-50\n");
        write_file(iio0.join("format/umask_ext"), "config1:0-31\n");

        let pmus = discover_pmus_from_sysfs(temp.root(), PmuKind::Iio).expect("PMUs");
        let packed = event
            .encode_for_linux(pmus[0].format())
            .expect("packed event");

        assert_eq!(
            packed.config,
            0x83 | (0x02 << 8) | (0x0ff << 36) | (0x07 << 48)
        );
        assert_eq!(packed.config1, 0);
        assert_eq!(packed.config2, 0);
    }

    #[test]
    fn marks_spr_peer_all_parts_as_derived_union() {
        let event = profile(PerfmonProfileId::SapphireRapids)
            .event(EventRole::IioDataReqOfCpuPeerWriteAllParts)
            .expect("derived SPR peer write all parts");

        assert_eq!(
            event
                .runtime_fields
                .iter()
                .find(|field| field.field == RuntimeField::PortMask)
                .expect("port mask")
                .value,
            0xff
        );
        assert!(matches!(
            event.provenance,
            EventProvenance::DerivedUnion {
                source_path: "SPR/events/sapphirerapids_uncore_experimental.json",
                source_event_names,
                rule,
                ..
            } if source_event_names.len() == 8
                && source_event_names[0] == "UNC_IIO_DATA_REQ_OF_CPU.PEER_WRITE.PART0"
                && rule.contains("OR PortMask")
        ));
    }

    #[test]
    fn preserves_counter_restrictions_in_profile_events() {
        let spr = profile(PerfmonProfileId::SapphireRapids);
        let upi = spr
            .event(EventRole::UpiTxDataFlitsAll)
            .expect("SPR UPI tx event");
        assert_eq!(
            upi.counter.counters,
            &[
                ProgrammableCounter::new(0),
                ProgrammableCounter::new(1),
                ProgrammableCounter::new(2),
                ProgrammableCounter::new(3)
            ]
        );

        let iio = spr
            .event(EventRole::IioDataReqOfCpuMemReadAllParts)
            .expect("SPR IIO mem read event");
        assert_eq!(
            iio.counter.counters,
            &[ProgrammableCounter::new(0), ProgrammableCounter::new(1)]
        );
    }

    #[test]
    fn exposes_operational_all_parts_inventory() {
        let spr = profile(PerfmonProfileId::SapphireRapids);
        assert!(
            spr.event(EventRole::IioDataReqOfCpuPeerWriteAllParts)
                .is_ok()
        );
        assert!(
            spr.event(EventRole::IioDataReqByCpuPeerWriteAllParts)
                .is_ok()
        );
        assert!(
            spr.event(EventRole::IioDataReqByCpuPeerReadAllParts)
                .is_ok()
        );
        assert!(
            spr.event(EventRole::IioDataReqOfCpuPeerReadAllParts)
                .is_err()
        );

        let gnr = profile(PerfmonProfileId::GraniteRapids);
        assert!(
            gnr.event(EventRole::IioDataReqOfCpuPeerWriteAllParts)
                .is_ok()
        );
        assert!(
            gnr.event(EventRole::IioDataReqOfCpuPeerReadAllParts)
                .is_ok()
        );
        assert!(
            gnr.event(EventRole::IioDataReqByCpuPeerWriteAllParts)
                .is_ok()
        );
        assert!(
            gnr.event(EventRole::IioDataReqByCpuPeerReadAllParts)
                .is_ok()
        );
    }

    #[test]
    fn attribution_points_to_pinned_commit_and_vendor_license() {
        assert_eq!(
            ATTRIBUTION.upstream_commit,
            "6e3329d20457aad11d8cc323b85aa6a16b075918"
        );
        assert_eq!(ATTRIBUTION.license_path, "vendor/intel-perfmon/LICENSE");
    }

    fn write_file(path: impl AsRef<Path>, contents: &str) {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent dir");
        }
        fs::write(path, contents).expect("write file");
    }

    struct TempSysfs {
        root: PathBuf,
    }

    impl TempSysfs {
        fn new() -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time")
                .as_nanos();
            let root = std::env::temp_dir()
                .join(format!("xfer-perfmon-test-{}-{unique}", std::process::id()));
            fs::create_dir_all(root.join("bus/event_source/devices")).expect("sysfs root");
            Self { root }
        }

        fn root(&self) -> &Path {
            &self.root
        }

        fn pmu(&self, name: &str) -> PathBuf {
            let path = self.root.join("bus/event_source/devices").join(name);
            fs::create_dir_all(&path).expect("pmu dir");
            path
        }
    }

    impl Drop for TempSysfs {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}
