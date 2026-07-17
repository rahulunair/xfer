//! Output model and formatting for list/bench results.
//!
//! Formatting is pure string construction. It does not read runtime config,
//! assets, terminal state, or environment variables.

use std::fmt;
use std::time::Duration;

use crate::cli::{OutputFormat, TimingMode, TransferClass};
use crate::histogram::Histogram;
use crate::stats::Summary;

pub const BENCH_CSV_HEADER: &str = "status,transfer_class,operation,peer_access,src_device,dst_device,bytes,size,allocation,queue_ordinal,queue_copy,queue_compute,timing_mode,warmup_ms,samples,negotiated_pcie_link,negotiated_pcie_theoretical_gb_s,median_gb_s,mad_gb_s,p5_gb_s,p95_gb_s,outliers_mild,outliers_severe,skip_reason";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextOptions {
    pub include_histogram: bool,
    pub color: ColorMode,
}

impl Default for TextOptions {
    fn default() -> Self {
        Self {
            include_histogram: true,
            color: ColorMode::Never,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColorMode {
    Never,
    Ansi,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ListReport {
    pub devices: Vec<DeviceInfo>,
    pub peer_access: Vec<PeerAccessInfo>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DeviceInfo {
    pub index: u32,
    pub name: String,
    pub pci_address: Option<String>,
    pub pcie_link: LinkInfo,
    pub queue_groups: Vec<QueueGroupInfo>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct QueueGroupInfo {
    pub ordinal: u32,
    pub flags: QueueFlags,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct QueueFlags {
    pub copy: bool,
    pub compute: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum LinkInfo {
    Known {
        generation: u8,
        width: u16,
        theoretical_gb_s: f64,
    },
    Unknown {
        reason: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PeerAccessInfo {
    pub from_device: u32,
    pub to_device: u32,
    pub access: PeerAccess,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PeerAccess {
    Yes,
    No,
    Unknown(String),
}

impl PeerAccess {
    pub fn as_field(&self) -> &str {
        match self {
            Self::Yes => "yes",
            Self::No => "no",
            Self::Unknown(_) => "unknown",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BenchReport {
    pub cases: Vec<BenchCase>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BenchCase {
    pub transfer_class: TransferClass,
    pub operation: Operation,
    pub source: Endpoint,
    pub destination: Endpoint,
    pub byte_count: u64,
    pub allocation: AllocationKind,
    pub queue: QueueGroupInfo,
    pub timing: TimingMode,
    pub warmup: Duration,
    pub requested_samples: u32,
    pub pcie_link: LinkInfo,
    pub outcome: CaseOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Operation {
    HostToDevice,
    DeviceToHost,
    SameDevice,
    Direct { peer_access: PeerAccess },
    ExplicitStaged,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Endpoint {
    Host,
    Device(u32),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AllocationKind {
    PinnedHost,
    Device,
    PinnedStaging,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CaseOutcome {
    Measured {
        summary: Summary,
        samples_gb_s: Vec<f64>,
    },
    Skipped {
        reason: String,
    },
}

pub fn render_list(report: &ListReport) -> String {
    let mut lines = Vec::new();

    if report.devices.is_empty() {
        lines.push("no Level Zero GPU devices found".to_owned());
    }

    for device in &report.devices {
        lines.push(format!(
            "dev{}  {}  [{}]",
            device.index,
            device.name,
            render_link_inline(&device.pcie_link)
        ));
        if let Some(address) = &device.pci_address {
            lines.push(format!("  pci {address}"));
        }

        for queue in &device.queue_groups {
            lines.push(format!(
                "  ordinal {} ({})",
                queue.ordinal,
                render_queue_flags(queue.flags)
            ));
        }
    }

    if !report.peer_access.is_empty() {
        lines.push(String::new());
        lines.push("peer access capability (zeDeviceCanAccessPeer)".to_owned());
        for peer in &report.peer_access {
            let reason = match &peer.access {
                PeerAccess::Unknown(reason) => format!(" ({reason})"),
                PeerAccess::Yes | PeerAccess::No => String::new(),
            };
            lines.push(format!(
                "  dev{} -> dev{}  {}{}",
                peer.from_device,
                peer.to_device,
                peer.access.as_field(),
                reason
            ));
        }
    }

    finish_lines(lines)
}

pub fn render_bench(report: &BenchReport, format: OutputFormat, text: &TextOptions) -> String {
    match format {
        OutputFormat::Text => render_bench_text(report, text),
        OutputFormat::Csv => render_bench_csv(report),
    }
}

pub fn render_bench_text(report: &BenchReport, options: &TextOptions) -> String {
    let mut lines = Vec::new();

    if report.cases.is_empty() {
        lines.push("no benchmark cases selected".to_owned());
        return finish_lines(lines);
    }

    for (case_index, case) in report.cases.iter().enumerate() {
        if case_index > 0 {
            lines.push(String::new());
        }

        lines.push(format!(
            "{} {} {} -> {}  [{}]",
            render_transfer_label(case.transfer_class),
            render_operation_label(&case.operation),
            case.source,
            case.destination,
            render_link_inline(&case.pcie_link)
        ));
        lines.push(format!(
            "  {}  ordinal {} ({})  timing {}  warmup {}  samples {}",
            format_bytes(case.byte_count),
            case.queue.ordinal,
            render_queue_flags(case.queue.flags),
            case.timing,
            format_duration(case.warmup),
            case.requested_samples
        ));
        lines.push(format!(
            "    transfer_class {}  bytes {}  allocation {}",
            case.transfer_class, case.byte_count, case.allocation
        ));
        if let Operation::Direct { peer_access } = &case.operation {
            lines.push(format!(
                "    direct_observed one Level Zero copy  peer_access_capability {}",
                peer_access.as_field()
            ));
        }

        match &case.outcome {
            CaseOutcome::Measured {
                summary,
                samples_gb_s,
            } => {
                lines.push(format!(
                    "    median  {} GB/s{}",
                    format_rate(summary.median),
                    render_negotiated_pcie_percent(summary.median, &case.pcie_link)
                ));
                lines.push(format!("    MAD     {} GB/s", format_rate(summary.mad)));
                lines.push(format!(
                    "    p5/p95  {} / {} GB/s",
                    format_rate(summary.p5),
                    format_rate(summary.p95)
                ));
                lines.push(format!(
                    "    outliers {}/{}: {} mild, {} severe",
                    summary.outliers.counts.mild + summary.outliers.counts.severe,
                    summary.count,
                    summary.outliers.counts.mild,
                    summary.outliers.counts.severe
                ));

                if options.include_histogram {
                    if let Some(histogram) = Histogram::from_samples(samples_gb_s, 12) {
                        if let Some(rows) = histogram.render_ascii(18, Some(summary.median)) {
                            lines.push(String::new());
                            for row in rows {
                                lines.push(format!("    {row}"));
                            }
                        }
                    }
                }
            }
            CaseOutcome::Skipped { reason } => {
                lines.push(format!("    skipped  {reason}"));
            }
        }
    }

    finish_lines(lines)
}

pub fn render_bench_csv(report: &BenchReport) -> String {
    let mut lines = Vec::with_capacity(report.cases.len() + 1);
    lines.push(BENCH_CSV_HEADER.to_owned());

    for case in &report.cases {
        lines.push(render_case_csv(case));
    }

    finish_lines(lines)
}

pub fn csv_escape(value: &str) -> String {
    if value
        .bytes()
        .any(|byte| matches!(byte, b',' | b'"' | b'\n' | b'\r'))
    {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

fn render_case_csv(case: &BenchCase) -> String {
    let mut fields = Vec::new();
    let (status, summary, skip_reason) = match &case.outcome {
        CaseOutcome::Measured { summary, .. } => ("measured", Some(summary), ""),
        CaseOutcome::Skipped { reason } => ("skipped", None, reason.as_str()),
    };

    fields.push(status.to_owned());
    fields.push(case.transfer_class.to_string());
    fields.push(render_operation_field(&case.operation).to_owned());
    fields.push(render_peer_access_field(&case.operation).to_owned());
    fields.push(render_endpoint_field(&case.source));
    fields.push(render_endpoint_field(&case.destination));
    fields.push(case.byte_count.to_string());
    fields.push(format_bytes(case.byte_count));
    fields.push(case.allocation.to_string());
    fields.push(case.queue.ordinal.to_string());
    fields.push(case.queue.flags.copy.to_string());
    fields.push(case.queue.flags.compute.to_string());
    fields.push(case.timing.to_string());
    fields.push(case.warmup.as_millis().to_string());
    fields.push(case.requested_samples.to_string());
    fields.push(render_link_field(&case.pcie_link));
    fields.push(render_link_theoretical_field(&case.pcie_link));

    if let Some(summary) = summary {
        fields.push(format_float(summary.median));
        fields.push(format_float(summary.mad));
        fields.push(format_float(summary.p5));
        fields.push(format_float(summary.p95));
        fields.push(summary.outliers.counts.mild.to_string());
        fields.push(summary.outliers.counts.severe.to_string());
    } else {
        for _ in 0..6 {
            fields.push(String::new());
        }
    }

    fields.push(skip_reason.to_owned());

    fields
        .into_iter()
        .map(|field| csv_escape(&field))
        .collect::<Vec<_>>()
        .join(",")
}

fn render_transfer_label(transfer_class: TransferClass) -> &'static str {
    match transfer_class {
        TransferClass::H2D => "H2D",
        TransferClass::D2H => "D2H",
        TransferClass::D2DSameDevice => "D2D same-device",
        TransferClass::D2DDirect => "D2D direct",
        TransferClass::D2DStaged => "D2D staged",
    }
}

fn render_operation_label(operation: &Operation) -> String {
    match operation {
        Operation::HostToDevice => "pinned".to_owned(),
        Operation::DeviceToHost => "pinned".to_owned(),
        Operation::SameDevice => "same-device".to_owned(),
        Operation::Direct { peer_access } => {
            format!("direct, peer-access={}", peer_access.as_field())
        }
        Operation::ExplicitStaged => "explicit-staged".to_owned(),
    }
}

fn render_operation_field(operation: &Operation) -> &'static str {
    match operation {
        Operation::HostToDevice => "h2d-pinned",
        Operation::DeviceToHost => "d2h-pinned",
        Operation::SameDevice => "same-device",
        Operation::Direct { .. } => "direct",
        Operation::ExplicitStaged => "explicit-staged",
    }
}

fn render_peer_access_field(operation: &Operation) -> &str {
    match operation {
        Operation::Direct { peer_access } => peer_access.as_field(),
        Operation::HostToDevice
        | Operation::DeviceToHost
        | Operation::SameDevice
        | Operation::ExplicitStaged => "",
    }
}

fn render_endpoint_field(endpoint: &Endpoint) -> String {
    endpoint.to_string()
}

fn render_link_inline(link: &LinkInfo) -> String {
    match link {
        LinkInfo::Known {
            generation,
            width,
            theoretical_gb_s,
        } => format!(
            "negotiated PCIe Gen{} x{}, {} GB/s theoretical",
            generation,
            width,
            format_rate(*theoretical_gb_s)
        ),
        LinkInfo::Unknown { reason } => format!("PCIe unknown: {reason}"),
    }
}

fn render_link_field(link: &LinkInfo) -> String {
    match link {
        LinkInfo::Known {
            generation, width, ..
        } => format!("Gen{generation}x{width}"),
        LinkInfo::Unknown { reason } => format!("unknown:{reason}"),
    }
}

fn render_link_theoretical_field(link: &LinkInfo) -> String {
    match link {
        LinkInfo::Known {
            theoretical_gb_s, ..
        } => format_float(*theoretical_gb_s),
        LinkInfo::Unknown { .. } => String::new(),
    }
}

fn render_negotiated_pcie_percent(rate_gb_s: f64, link: &LinkInfo) -> String {
    match link {
        LinkInfo::Known {
            theoretical_gb_s, ..
        } if *theoretical_gb_s > 0.0 => {
            format!(
                "  ({}% of negotiated PCIe theoretical)",
                (rate_gb_s / theoretical_gb_s * 100.0).round()
            )
        }
        LinkInfo::Known { .. } | LinkInfo::Unknown { .. } => String::new(),
    }
}

fn render_queue_flags(flags: QueueFlags) -> String {
    match (flags.copy, flags.compute) {
        (true, true) => "copy+compute".to_owned(),
        (true, false) => "copy".to_owned(),
        (false, true) => "compute".to_owned(),
        (false, false) => "no-copy-no-compute".to_owned(),
    }
}

fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    const TIB: u64 = GIB * 1024;

    for (unit, factor) in [("TiB", TIB), ("GiB", GIB), ("MiB", MIB), ("KiB", KIB)] {
        if bytes >= factor && bytes % factor == 0 {
            return format!("{} {unit}", bytes / factor);
        }
    }

    format!("{bytes} bytes")
}

fn format_duration(duration: Duration) -> String {
    if duration.as_micros() == 0 {
        return "0s".to_owned();
    }
    if duration.subsec_nanos() == 0 {
        return format!("{}s", duration.as_secs());
    }
    if duration.as_millis() * 1_000_000 == duration.as_nanos() {
        return format!("{}ms", duration.as_millis());
    }
    if duration.as_micros() * 1_000 == duration.as_nanos() {
        return format!("{}us", duration.as_micros());
    }

    format!("{}ns", duration.as_nanos())
}

fn format_rate(value: f64) -> String {
    format!("{value:.1}")
}

fn format_float(value: f64) -> String {
    if value.is_finite() {
        format!("{value:.6}")
    } else {
        String::new()
    }
}

fn finish_lines(lines: Vec<String>) -> String {
    let mut output = lines.join("\n");
    output.push('\n');
    output
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Host => f.write_str("host"),
            Self::Device(index) => write!(f, "dev{index}"),
        }
    }
}

impl fmt::Display for AllocationKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PinnedHost => f.write_str("pinned-host"),
            Self::Device => f.write_str("device"),
            Self::PinnedStaging => f.write_str("pinned-staging"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats;

    fn measured_case() -> BenchCase {
        let samples = vec![49.8, 50.7, 51.2, 51.6, 51.9];
        let summary = stats::summarize(&samples).expect("summary");

        BenchCase {
            transfer_class: TransferClass::D2H,
            operation: Operation::DeviceToHost,
            source: Endpoint::Device(0),
            destination: Endpoint::Host,
            byte_count: 256 * 1024 * 1024,
            allocation: AllocationKind::PinnedHost,
            queue: QueueGroupInfo {
                ordinal: 1,
                flags: QueueFlags {
                    copy: true,
                    compute: false,
                },
            },
            timing: TimingMode::WallClock,
            warmup: Duration::from_secs(1),
            requested_samples: 5,
            pcie_link: LinkInfo::Known {
                generation: 5,
                width: 16,
                theoretical_gb_s: 63.015_384,
            },
            outcome: CaseOutcome::Measured {
                summary,
                samples_gb_s: samples,
            },
        }
    }

    #[test]
    fn renders_dense_text_with_reproducibility_fields() {
        let report = BenchReport {
            cases: vec![measured_case()],
        };
        let output = render_bench_text(&report, &TextOptions::default());

        assert!(output.contains(
            "D2H pinned dev0 -> host  [negotiated PCIe Gen5 x16, 63.0 GB/s theoretical]"
        ));
        assert!(output.contains("256 MiB  ordinal 1 (copy)  timing wall-clock"));
        assert!(output.contains("transfer_class d2h  bytes 268435456  allocation pinned-host"));
        assert!(output.contains("median  51.2 GB/s  (81% of negotiated PCIe theoretical)"));
        assert!(output.contains("p5/p95"));
        assert!(output.contains("median"));
        assert!(!output.contains("\u{1b}["));
    }

    #[test]
    fn omits_histogram_when_requested() {
        let report = BenchReport {
            cases: vec![measured_case()],
        };
        let output = render_bench_text(
            &report,
            &TextOptions {
                include_histogram: false,
                color: ColorMode::Never,
            },
        );

        assert!(!output.contains(" | #"));
    }

    #[test]
    fn renders_skipped_cases_with_reason() {
        let report = BenchReport {
            cases: vec![BenchCase {
                transfer_class: TransferClass::D2DDirect,
                operation: Operation::Direct {
                    peer_access: PeerAccess::No,
                },
                source: Endpoint::Device(0),
                destination: Endpoint::Device(1),
                byte_count: 1024,
                allocation: AllocationKind::Device,
                queue: QueueGroupInfo {
                    ordinal: 0,
                    flags: QueueFlags {
                        copy: true,
                        compute: true,
                    },
                },
                timing: TimingMode::DeviceTimestamps,
                warmup: Duration::from_millis(250),
                requested_samples: 10,
                pcie_link: LinkInfo::Unknown {
                    reason: "missing sysfs mapping".to_owned(),
                },
                outcome: CaseOutcome::Skipped {
                    reason: "peer access unsupported".to_owned(),
                },
            }],
        };

        let text = render_bench_text(&report, &TextOptions::default());
        assert!(text.contains("direct, peer-access=no dev0 -> dev1"));
        assert!(text.contains("direct_observed one Level Zero copy  peer_access_capability no"));
        assert!(text.contains("skipped  peer access unsupported"));

        let csv = render_bench_csv(&report);
        assert!(csv.contains("skipped,d2d-direct,direct,no,dev0,dev1"));
        assert!(csv.contains("peer access unsupported"));
    }

    #[test]
    fn csv_header_is_stable() {
        let columns = BENCH_CSV_HEADER.split(',').collect::<Vec<_>>();

        assert_eq!(columns.len(), 24);
        assert_eq!(columns[0], "status");
        assert_eq!(columns[1], "transfer_class");
        assert_eq!(columns[23], "skip_reason");
    }

    #[test]
    fn csv_escaping_handles_commas_quotes_and_newlines() {
        assert_eq!(csv_escape("plain"), "plain");
        assert_eq!(csv_escape("a,b"), "\"a,b\"");
        assert_eq!(csv_escape("a\"b"), "\"a\"\"b\"");
        assert_eq!(csv_escape("a\nb"), "\"a\nb\"");
    }

    #[test]
    fn renders_measured_csv_with_stable_field_count_and_escaping() {
        let mut case = measured_case();
        case.pcie_link = LinkInfo::Unknown {
            reason: "bad, \"quoted\" path".to_owned(),
        };
        let report = BenchReport { cases: vec![case] };

        let csv = render_bench_csv(&report);
        let lines = csv.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], BENCH_CSV_HEADER);
        assert_eq!(
            split_csv_record(lines[0]).len(),
            split_csv_record(lines[1]).len()
        );
        assert!(lines[1].contains("\"unknown:bad, \"\"quoted\"\" path\""));
    }

    #[test]
    fn renders_list_text() {
        let report = ListReport {
            devices: vec![DeviceInfo {
                index: 0,
                name: "Intel GPU".to_owned(),
                pci_address: Some("0000:03:00.0".to_owned()),
                pcie_link: LinkInfo::Known {
                    generation: 5,
                    width: 16,
                    theoretical_gb_s: 63.015_384,
                },
                queue_groups: vec![QueueGroupInfo {
                    ordinal: 1,
                    flags: QueueFlags {
                        copy: true,
                        compute: false,
                    },
                }],
            }],
            peer_access: vec![PeerAccessInfo {
                from_device: 0,
                to_device: 1,
                access: PeerAccess::Yes,
            }],
        };

        let text = render_list(&report);
        assert!(text.contains("dev0  Intel GPU"));
        assert!(text.contains("ordinal 1 (copy)"));
        assert!(text.contains("dev0 -> dev1  yes"));
    }

    fn split_csv_record(line: &str) -> Vec<String> {
        let mut fields = Vec::new();
        let mut field = String::new();
        let mut chars = line.chars().peekable();
        let mut quoted = false;

        while let Some(ch) = chars.next() {
            match ch {
                '"' if quoted && chars.peek() == Some(&'"') => {
                    field.push('"');
                    chars.next();
                }
                '"' => quoted = !quoted,
                ',' if !quoted => {
                    fields.push(field);
                    field = String::new();
                }
                _ => field.push(ch),
            }
        }

        fields.push(field);
        fields
    }
}
