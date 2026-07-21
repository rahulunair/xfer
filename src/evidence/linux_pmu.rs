use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use super::error::{EvidenceError, Result, read_error};

const SYSFS_EVENT_SOURCE_DEVICES: &str = "bus/event_source/devices";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PmuKind {
    Iio,
    UpiLl,
}

impl PmuKind {
    fn linux_prefix(self) -> &'static str {
        match self {
            Self::Iio => "uncore_iio",
            Self::UpiLl => "uncore_upi",
        }
    }

    fn matches_name(self, name: &str) -> bool {
        let prefix = self.linux_prefix();
        if name == prefix {
            return true;
        }
        let Some(rest) = name
            .strip_prefix(prefix)
            .and_then(|rest| rest.strip_prefix('_'))
        else {
            return false;
        };
        !rest.is_empty() && rest.bytes().all(|byte| byte.is_ascii_digit())
    }
}

impl fmt::Display for PmuKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Iio => f.write_str("uncore IIO"),
            Self::UpiLl => f.write_str("uncore UPI"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PmuType(u32);

impl PmuType {
    pub fn new(value: u32) -> Self {
        Self(value)
    }

    pub fn value(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct PmuInstanceId(u32);

impl PmuInstanceId {
    pub fn new(value: u32) -> Self {
        Self(value)
    }

    pub fn value(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct FormatFieldName(String);

impl FormatFieldName {
    pub fn new(name: impl Into<String>) -> Result<Self> {
        let name = name.into();
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err(EvidenceError::MalformedSysfs {
                path: PathBuf::from("format"),
                reason: format!("invalid PMU format field name '{name}'"),
            });
        }
        Ok(Self(name))
    }

    pub fn event() -> Self {
        Self("event".to_owned())
    }

    pub fn umask() -> Self {
        Self("umask".to_owned())
    }

    pub fn ch_mask() -> Self {
        Self("ch_mask".to_owned())
    }

    pub fn fc_mask() -> Self {
        Self("fc_mask".to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for FormatFieldName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct PmuAliasName(String);

impl PmuAliasName {
    pub fn new(name: impl Into<String>) -> Result<Self> {
        let name = name.into();
        if name.is_empty() {
            return Err(EvidenceError::MalformedSysfs {
                path: PathBuf::from("events"),
                reason: "empty PMU alias name".to_owned(),
            });
        }
        Ok(Self(name))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ConfigRegister {
    Config,
    Config1,
    Config2,
}

impl ConfigRegister {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "config" => Some(Self::Config),
            "config1" => Some(Self::Config1),
            "config2" => Some(Self::Config2),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BitRange {
    start: u8,
    end: u8,
}

impl BitRange {
    fn width(self) -> u8 {
        self.end - self.start + 1
    }

    fn bits(self) -> impl Iterator<Item = u8> {
        self.start..=self.end
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FormatField {
    name: FormatFieldName,
    register: ConfigRegister,
    ranges: Vec<BitRange>,
}

impl FormatField {
    pub fn name(&self) -> &FormatFieldName {
        &self.name
    }

    pub fn register(&self) -> ConfigRegister {
        self.register
    }

    pub fn width(&self) -> u8 {
        self.ranges.iter().map(|range| range.width()).sum()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PmuFormat {
    fields: BTreeMap<FormatFieldName, FormatField>,
}

impl PmuFormat {
    pub fn new(fields: Vec<FormatField>) -> Result<Self> {
        validate_non_overlapping_fields(&fields)?;
        let mut by_name = BTreeMap::new();
        for field in fields {
            if by_name.insert(field.name.clone(), field).is_some() {
                return Err(EvidenceError::MalformedSysfs {
                    path: PathBuf::from("format"),
                    reason: "duplicate PMU format field".to_owned(),
                });
            }
        }
        Ok(Self { fields: by_name })
    }

    pub fn fields(&self) -> impl Iterator<Item = &FormatField> {
        self.fields.values()
    }

    pub fn field(&self, name: &FormatFieldName) -> Option<&FormatField> {
        self.fields.get(name)
    }

    pub fn pack(&self, values: &[PmuFieldValue]) -> Result<PackedConfig> {
        validate_unique_values(values)?;
        let mut packed = PackedConfig::default();
        for value in values {
            let field =
                self.field(&value.field)
                    .ok_or_else(|| EvidenceError::MissingFormatField {
                        field: value.field.clone(),
                    })?;
            pack_field(field, value.value, &mut packed)?;
        }
        Ok(packed)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PackedConfig {
    pub config: u64,
    pub config1: u64,
    pub config2: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PmuFieldValue {
    field: FormatFieldName,
    value: u64,
}

impl PmuFieldValue {
    pub fn new(field: FormatFieldName, value: u64) -> Self {
        Self { field, value }
    }

    pub fn field(&self) -> &FormatFieldName {
        &self.field
    }

    pub fn value(&self) -> u64 {
        self.value
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PmuAlias {
    name: PmuAliasName,
    encoding: String,
    scale: Option<String>,
    unit: Option<String>,
}

impl PmuAlias {
    pub fn name(&self) -> &PmuAliasName {
        &self.name
    }

    pub fn encoding(&self) -> &str {
        &self.encoding
    }

    pub fn scale(&self) -> Option<&str> {
        self.scale.as_deref()
    }

    pub fn unit(&self) -> Option<&str> {
        self.unit.as_deref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PmuInstance {
    kind: PmuKind,
    name: String,
    instance: Option<PmuInstanceId>,
    pmu_type: PmuType,
    cpumask: Option<String>,
    format: PmuFormat,
    aliases: Vec<PmuAlias>,
}

impl PmuInstance {
    pub fn kind(&self) -> PmuKind {
        self.kind
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn instance(&self) -> Option<PmuInstanceId> {
        self.instance
    }

    pub fn pmu_type(&self) -> PmuType {
        self.pmu_type
    }

    pub fn cpumask(&self) -> Option<&str> {
        self.cpumask.as_deref()
    }

    pub fn format(&self) -> &PmuFormat {
        &self.format
    }

    pub fn aliases(&self) -> &[PmuAlias] {
        &self.aliases
    }
}

pub fn discover_pmus(kind: PmuKind) -> Result<Vec<PmuInstance>> {
    discover_pmus_from_sysfs(Path::new("/sys"), kind)
}

pub fn discover_pmus_from_sysfs(
    sysfs_root: impl AsRef<Path>,
    kind: PmuKind,
) -> Result<Vec<PmuInstance>> {
    let devices = sysfs_root.as_ref().join(SYSFS_EVENT_SOURCE_DEVICES);
    let entries = fs::read_dir(&devices).map_err(|error| read_error(&devices, error))?;
    let mut instances = Vec::new();

    for entry in entries {
        let entry = entry.map_err(|error| read_error(&devices, error))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !kind.matches_name(&name) {
            continue;
        }
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        instances.push(read_pmu_instance(kind, &name, &path)?);
    }

    if instances.is_empty() {
        return Err(EvidenceError::MissingPmu { kind });
    }

    instances.sort_by(|left, right| {
        left.instance
            .cmp(&right.instance)
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(instances)
}

fn read_pmu_instance(kind: PmuKind, name: &str, path: &Path) -> Result<PmuInstance> {
    let pmu_type = read_pmu_type(path)?;
    let cpumask = read_optional_trimmed(path.join("cpumask"))?;
    let format = read_pmu_format(path)?;
    let aliases = read_aliases(path)?;

    Ok(PmuInstance {
        kind,
        name: name.to_owned(),
        instance: parse_instance_id(kind, name),
        pmu_type,
        cpumask,
        format,
        aliases,
    })
}

fn read_pmu_type(path: &Path) -> Result<PmuType> {
    let type_path = path.join("type");
    let text = read_trimmed(&type_path)?;
    let value = text
        .parse::<u32>()
        .map_err(|error| EvidenceError::MalformedSysfs {
            path: type_path.clone(),
            reason: format!("PMU type is not u32: {error}"),
        })?;
    Ok(PmuType::new(value))
}

fn read_pmu_format(path: &Path) -> Result<PmuFormat> {
    let format_path = path.join("format");
    let entries = fs::read_dir(&format_path).map_err(|error| read_error(&format_path, error))?;
    let mut fields = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| read_error(&format_path, error))?;
        let field_path = entry.path();
        if !field_path.is_file() {
            continue;
        }
        let name = FormatFieldName::new(entry.file_name().to_string_lossy().into_owned())?;
        let spec = read_trimmed(&field_path)?;
        fields.push(parse_format_field(name, &spec, &field_path)?);
    }
    PmuFormat::new(fields)
}

fn read_aliases(path: &Path) -> Result<Vec<PmuAlias>> {
    let events_path = path.join("events");
    let entries = match fs::read_dir(&events_path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(read_error(&events_path, error)),
    };

    let mut base_aliases = BTreeMap::<String, String>::new();
    let mut scales = BTreeMap::<String, String>::new();
    let mut units = BTreeMap::<String, String>::new();
    for entry in entries {
        let entry = entry.map_err(|error| read_error(&events_path, error))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let value = read_trimmed(&path)?;
        if let Some(alias) = name.strip_suffix(".scale") {
            scales.insert(alias.to_owned(), value);
        } else if let Some(alias) = name.strip_suffix(".unit") {
            units.insert(alias.to_owned(), value);
        } else {
            base_aliases.insert(name, value);
        }
    }

    base_aliases
        .into_iter()
        .map(|(name, encoding)| {
            Ok(PmuAlias {
                scale: scales.remove(&name),
                unit: units.remove(&name),
                name: PmuAliasName::new(name)?,
                encoding,
            })
        })
        .collect()
}

fn parse_instance_id(kind: PmuKind, name: &str) -> Option<PmuInstanceId> {
    let rest = name.strip_prefix(kind.linux_prefix())?.strip_prefix('_')?;
    if rest.is_empty() || !rest.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    rest.parse::<u32>().ok().map(PmuInstanceId::new)
}

fn parse_format_field(name: FormatFieldName, spec: &str, path: &Path) -> Result<FormatField> {
    let Some((register, bits)) = spec.split_once(':') else {
        return Err(EvidenceError::MalformedSysfs {
            path: path.to_path_buf(),
            reason: format!("format spec '{spec}' is missing ':'"),
        });
    };
    let Some(register) = ConfigRegister::parse(register) else {
        return Err(EvidenceError::MalformedSysfs {
            path: path.to_path_buf(),
            reason: format!("unsupported config register '{register}'"),
        });
    };

    let mut ranges = Vec::new();
    for bit_spec in bits.split(',') {
        // Linux perf maps split fields in the textual order provided by
        // format/*; preserve that ABI order when consuming value bits.
        ranges.push(parse_bit_range(bit_spec, path)?);
    }
    if ranges.is_empty() {
        return Err(EvidenceError::MalformedSysfs {
            path: path.to_path_buf(),
            reason: "format field has no bit ranges".to_owned(),
        });
    }
    Ok(FormatField {
        name,
        register,
        ranges,
    })
}

fn parse_bit_range(spec: &str, path: &Path) -> Result<BitRange> {
    let parse_bit = |text: &str| {
        text.parse::<u8>()
            .ok()
            .filter(|bit| *bit < 64)
            .ok_or_else(|| EvidenceError::MalformedSysfs {
                path: path.to_path_buf(),
                reason: format!("invalid bit index '{text}'"),
            })
    };

    let (start, end) = if let Some((start, end)) = spec.split_once('-') {
        (parse_bit(start)?, parse_bit(end)?)
    } else {
        let bit = parse_bit(spec)?;
        (bit, bit)
    };
    if start > end {
        return Err(EvidenceError::MalformedSysfs {
            path: path.to_path_buf(),
            reason: format!("bit range '{spec}' starts after it ends"),
        });
    }
    Ok(BitRange { start, end })
}

fn validate_non_overlapping_fields(fields: &[FormatField]) -> Result<()> {
    let mut occupied = BTreeSet::new();
    for field in fields {
        for range in &field.ranges {
            for bit in range.bits() {
                if !occupied.insert((field.register, bit)) {
                    return Err(EvidenceError::MalformedSysfs {
                        path: PathBuf::from("format"),
                        reason: format!("overlapping PMU format bit {bit} in {:?}", field.register),
                    });
                }
            }
        }
    }
    Ok(())
}

fn validate_unique_values(values: &[PmuFieldValue]) -> Result<()> {
    let mut seen = BTreeSet::new();
    for value in values {
        if !seen.insert(value.field.clone()) {
            return Err(EvidenceError::MalformedSysfs {
                path: PathBuf::from("format"),
                reason: format!("duplicate PMU field value for {}", value.field),
            });
        }
    }
    Ok(())
}

fn pack_field(field: &FormatField, value: u64, packed: &mut PackedConfig) -> Result<()> {
    let width = field.width();
    if width < 64 && value >= (1_u64 << width) {
        return Err(EvidenceError::FieldValueTooLarge {
            field: field.name.clone(),
            value,
            width,
        });
    }

    let mut consumed = 0_u8;
    for range in &field.ranges {
        let mask = low_bits(range.width());
        let chunk = (value >> consumed) & mask;
        let shifted = chunk << range.start;
        match field.register {
            ConfigRegister::Config => packed.config |= shifted,
            ConfigRegister::Config1 => packed.config1 |= shifted,
            ConfigRegister::Config2 => packed.config2 |= shifted,
        }
        consumed += range.width();
    }
    Ok(())
}

fn low_bits(width: u8) -> u64 {
    if width == 64 {
        u64::MAX
    } else {
        (1_u64 << width) - 1
    }
}

fn read_trimmed(path: &Path) -> Result<String> {
    fs::read_to_string(path)
        .map(|text| text.trim().to_owned())
        .map_err(|error| read_error(path, error))
}

fn read_optional_trimmed(path: PathBuf) -> Result<Option<String>> {
    match fs::read_to_string(&path) {
        Ok(text) => Ok(Some(text.trim().to_owned())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(read_error(path, error)),
    }
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn packs_fields_from_discovered_format_positions() {
        let format = PmuFormat::new(vec![
            field("event", ConfigRegister::Config, &[(0, 7)]),
            field("umask", ConfigRegister::Config, &[(8, 15)]),
            field("ch_mask", ConfigRegister::Config, &[(36, 47)]),
            field("fc_mask", ConfigRegister::Config, &[(48, 50)]),
        ])
        .expect("format");

        let packed = format
            .pack(&[
                PmuFieldValue::new(FormatFieldName::event(), 0x83),
                PmuFieldValue::new(FormatFieldName::umask(), 0x04),
                PmuFieldValue::new(FormatFieldName::ch_mask(), 0xff),
                PmuFieldValue::new(FormatFieldName::fc_mask(), 0x07),
            ])
            .expect("packed");

        assert_eq!(
            packed.config,
            0x83 | (0x04 << 8) | (0xff << 36) | (0x07 << 48)
        );
        assert_eq!(packed.config1, 0);
        assert_eq!(packed.config2, 0);
    }

    #[test]
    fn rejects_overlapping_format_fields() {
        let error = PmuFormat::new(vec![
            field("event", ConfigRegister::Config, &[(0, 7)]),
            field("umask", ConfigRegister::Config, &[(4, 11)]),
        ])
        .expect_err("overlap");

        assert!(matches!(error, EvidenceError::MalformedSysfs { .. }));
    }

    #[test]
    fn rejects_malformed_format_specs() {
        let path = Path::new("/fake/format/event");
        let error = parse_format_field(FormatFieldName::event(), "config:7-0", path)
            .expect_err("bad range");
        assert!(matches!(error, EvidenceError::MalformedSysfs { .. }));

        let error = parse_format_field(FormatFieldName::event(), "bad:0-7", path)
            .expect_err("bad register");
        assert!(matches!(error, EvidenceError::MalformedSysfs { .. }));
    }

    #[test]
    fn rejects_values_too_wide_for_format_field() {
        let format = PmuFormat::new(vec![field("event", ConfigRegister::Config, &[(0, 7)])])
            .expect("format");

        let error = format
            .pack(&[PmuFieldValue::new(FormatFieldName::event(), 0x100)])
            .expect_err("wide value");

        assert!(matches!(error, EvidenceError::FieldValueTooLarge { .. }));
    }

    #[test]
    fn rejects_duplicate_field_values_and_ranges() {
        let format = PmuFormat::new(vec![field("event", ConfigRegister::Config, &[(0, 7)])])
            .expect("format");
        assert!(matches!(
            format.pack(&[
                PmuFieldValue::new(FormatFieldName::event(), 1),
                PmuFieldValue::new(FormatFieldName::event(), 2),
            ]),
            Err(EvidenceError::MalformedSysfs { .. })
        ));

        assert!(matches!(
            PmuFormat::new(vec![field(
                "event",
                ConfigRegister::Config,
                &[(0, 3), (3, 7)]
            )]),
            Err(EvidenceError::MalformedSysfs { .. })
        ));
    }

    #[test]
    fn split_ranges_pack_in_linux_textual_order() {
        let format = PmuFormat::new(vec![field(
            "split",
            ConfigRegister::Config,
            &[(8, 11), (0, 3)],
        )])
        .expect("format");
        let packed = format
            .pack(&[PmuFieldValue::new(
                FormatFieldName::new("split").expect("field"),
                0xab,
            )])
            .expect("packed");

        assert_eq!(packed.config, 0x0b00 | 0x0a);
    }

    #[test]
    fn discovers_pmu_instances_aliases_scales_and_cpumask() {
        let temp = TempSysfs::new();
        let iio0 = temp.pmu("uncore_iio_0");
        write_file(iio0.join("type"), "67\n");
        write_file(iio0.join("cpumask"), "0,56\n");
        write_file(iio0.join("format/event"), "config:0-7\n");
        write_file(iio0.join("format/umask"), "config:8-15\n");
        write_file(iio0.join("events/clockticks"), "event=0x1,umask=0x0\n");
        write_file(iio0.join("events/clockticks.scale"), "1\n");
        write_file(iio0.join("events/clockticks.unit"), "ticks\n");
        let ignored = temp.pmu("uncore_iio_free_running_0");
        write_file(ignored.join("type"), "68\n");

        let instances = discover_pmus_from_sysfs(temp.root(), PmuKind::Iio).expect("PMUs");

        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].instance(), Some(PmuInstanceId::new(0)));
        assert_eq!(instances[0].pmu_type(), PmuType::new(67));
        assert_eq!(instances[0].cpumask(), Some("0,56"));
        assert_eq!(instances[0].aliases()[0].name().as_str(), "clockticks");
        assert_eq!(instances[0].aliases()[0].scale(), Some("1"));
        assert_eq!(instances[0].aliases()[0].unit(), Some("ticks"));
    }

    #[test]
    fn distinguishes_missing_pmu_and_malformed_type() {
        let temp = TempSysfs::new();
        assert!(matches!(
            discover_pmus_from_sysfs(temp.root(), PmuKind::Iio),
            Err(EvidenceError::MissingPmu { kind: PmuKind::Iio })
        ));

        let iio0 = temp.pmu("uncore_iio_0");
        write_file(iio0.join("type"), "not-a-number\n");
        write_file(iio0.join("format/event"), "config:0-7\n");

        assert!(matches!(
            discover_pmus_from_sysfs(temp.root(), PmuKind::Iio),
            Err(EvidenceError::MalformedSysfs { .. })
        ));
    }

    #[test]
    fn ignores_invalid_suffixes_and_propagates_events_io_errors() {
        let temp = TempSysfs::new();
        let invalid = temp.pmu("uncore_iio_0abc");
        write_file(invalid.join("type"), "67\n");
        write_file(invalid.join("format/event"), "config:0-7\n");
        assert!(matches!(
            discover_pmus_from_sysfs(temp.root(), PmuKind::Iio),
            Err(EvidenceError::MissingPmu { kind: PmuKind::Iio })
        ));

        let iio0 = temp.pmu("uncore_iio_0");
        write_file(iio0.join("type"), "67\n");
        write_file(iio0.join("format/event"), "config:0-7\n");
        write_file(iio0.join("events"), "not a directory\n");
        assert!(matches!(
            discover_pmus_from_sysfs(temp.root(), PmuKind::Iio),
            Err(EvidenceError::Io { .. })
        ));
    }

    fn field(name: &str, register: ConfigRegister, ranges: &[(u8, u8)]) -> FormatField {
        FormatField {
            name: FormatFieldName::new(name).expect("field name"),
            register,
            ranges: ranges
                .iter()
                .map(|(start, end)| BitRange {
                    start: *start,
                    end: *end,
                })
                .collect(),
        }
    }

    fn write_file(path: impl AsRef<Path>, contents: &str) {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent dir");
        }
        let mut file = File::create(path).expect("create file");
        file.write_all(contents.as_bytes()).expect("write file");
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
            let root =
                std::env::temp_dir().join(format!("xfer-pmu-test-{}-{unique}", std::process::id()));
            fs::create_dir_all(root.join(SYSFS_EVENT_SOURCE_DEVICES)).expect("sysfs root");
            Self { root }
        }

        fn root(&self) -> &Path {
            &self.root
        }

        fn pmu(&self, name: &str) -> PathBuf {
            let path = self.root.join(SYSFS_EVENT_SOURCE_DEVICES).join(name);
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
