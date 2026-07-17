use super::model::{
    BenchCase, BenchReport, CaseOutcome, ColorMode, LinkInfo, ListReport, Operation, PeerAccess,
    QueueFlags, TextOptions,
};
use crate::cli::TransferClass;
use crate::histogram::Histogram;
use crate::stats::Summary;

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
                "  engine {} ({})",
                queue.ordinal,
                render_engine_flags(queue.flags)
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
        lines.extend(render_case_lines(case, options));
    }

    finish_lines(lines)
}

pub fn render_case_text(case: &BenchCase, options: &TextOptions) -> String {
    finish_lines(render_case_lines(case, options))
}

pub(crate) fn status_label_from_case_id(id: &str) -> String {
    let mut parts = id.split('/');
    let class = parts.next().unwrap_or_default();
    let endpoints = parts.next().unwrap_or_default();
    let _size = parts.next();
    let engine = parts
        .next()
        .and_then(|part| part.strip_prefix("engine-"))
        .map(|id| format!(" / engine {id}"))
        .unwrap_or_default();
    let class = match class {
        "h2d" => "H2D".to_owned(),
        "d2h" => "D2H".to_owned(),
        "d2d-same-device" => "D2D same-device".to_owned(),
        "d2d-direct" => "D2D direct".to_owned(),
        "d2d-staged" => "D2D explicit-staged".to_owned(),
        other => other.to_owned(),
    };
    let endpoints = endpoints.split_once("-to-").map_or_else(
        || endpoints.to_owned(),
        |(source, destination)| format!("{source} -> {destination}"),
    );

    if endpoints.is_empty() {
        format!("{class}{engine}")
    } else {
        format!("{class} {endpoints}{engine}")
    }
}

pub(crate) fn format_duration(duration: std::time::Duration) -> String {
    if duration.is_zero() {
        return "0 s".to_owned();
    }

    let seconds = duration.as_secs_f64();
    if seconds >= 1.0 {
        return format!("{} s", format_human_decimal(seconds, 2));
    }
    if seconds >= 1e-3 {
        return format!("{} ms", format_human_decimal(seconds * 1e3, 2));
    }
    if seconds >= 1e-6 {
        return format!("{} us", format_human_decimal(seconds * 1e6, 2));
    }

    format!("{} ns", format_human_decimal(duration.as_nanos() as f64, 2))
}

pub(crate) fn format_bytes(bytes: u64) -> String {
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

fn render_case_lines(case: &BenchCase, options: &TextOptions) -> Vec<String> {
    let mut lines = Vec::new();
    let label = case_label(case);
    lines.push(paint(options.color, "\u{1b}[1m", &label));
    lines.push(format!(
        "  {} on {} engine {}  [{}, {} samples, {} warm-up]",
        format_bytes(case.byte_count),
        render_engine_flags(case.queue.flags),
        case.queue.ordinal,
        render_timing(case.timing),
        case.requested_samples,
        format_duration(case.warmup)
    ));
    lines.push(format!("  {}", render_link_inline(&case.pcie_link)));
    lines.push(format!(
        "  config      class={} allocation={} bytes={} engine={}",
        case.transfer_class, case.allocation, case.byte_count, case.queue.ordinal
    ));

    if let Operation::Direct { peer_access } = &case.operation {
        lines.push(format!(
            "  path        direct Level Zero copy; peer access {}; physical route not inferred",
            peer_access.as_field()
        ));
    }

    match &case.outcome {
        CaseOutcome::Measured {
            time_summary,
            summary,
            samples_gb_s,
        } => {
            lines.extend(render_measured_lines(
                case,
                time_summary,
                summary,
                samples_gb_s,
                options,
            ));
        }
        CaseOutcome::Skipped { reason } => {
            lines.push(paint(
                options.color,
                "\u{1b}[33m",
                &format!("  skipped: {}", sanitize_human_text(reason)),
            ));
        }
    }

    lines
}

fn render_measured_lines(
    case: &BenchCase,
    time_summary: &Summary,
    summary: &Summary,
    samples_gb_s: &[f64],
    options: &TextOptions,
) -> Vec<String> {
    let mut lines = vec![String::new()];
    let time = format_seconds_triplet([
        time_summary.median_confidence.lower_bound,
        time_summary.median,
        time_summary.median_confidence.upper_bound,
    ]);
    let throughput = format_rate_triplet([
        summary.median_confidence.lower_bound,
        summary.median,
        summary.median_confidence.upper_bound,
    ]);
    let [heading, time, throughput] = estimate_table(time, throughput, options.color);
    lines.extend([heading, time, throughput]);
    lines.push(paint(
        options.color,
        "\u{1b}[2m",
        &format!(
            "{:16}{:.0}% bootstrap confidence interval ({} resamples)",
            "",
            summary.median_confidence.confidence_level * 100.0,
            summary.median_confidence.resamples
        ),
    ));

    let [p5, _, p95] = format_rate_triplet([summary.p5, summary.median, summary.p95]);
    lines.push(format!("  sample       p5 {p5}, p95 {p95}"));
    let resolution_note = if summary.mad == 0.0 {
        " (no variation at timer resolution)"
    } else {
        ""
    };
    lines.push(format!(
        "  variability  MAD {} GB/s{resolution_note}",
        format_nonzero_metric(summary.mad, 1)
    ));
    if let Some(spec) = render_negotiated_pcie_percent(summary.median, &case.pcie_link) {
        lines.push(format!("  link usage   {spec}"));
    }

    let outlier_line = format!(
        "  outliers     {}/{} ({} mild, {} severe)",
        time_summary.outliers.counts.mild + time_summary.outliers.counts.severe,
        time_summary.count,
        time_summary.outliers.counts.mild,
        time_summary.outliers.counts.severe
    );
    let outlier_color = if time_summary.outliers.counts.severe > 0 {
        "\u{1b}[31m"
    } else if time_summary.outliers.counts.mild > 0 {
        "\u{1b}[33m"
    } else {
        "\u{1b}[2m"
    };
    lines.push(paint(options.color, outlier_color, &outlier_line));

    if options.include_histogram {
        append_histogram(&mut lines, samples_gb_s, summary.median, options.color);
    }
    lines
}

fn append_histogram(lines: &mut Vec<String>, samples: &[f64], median: f64, color: ColorMode) {
    let Some(histogram) = Histogram::from_samples(samples, 12) else {
        return;
    };
    let Some(rows) = render_histogram(&histogram, Some(median), color) else {
        return;
    };

    lines.push(String::new());
    lines.push(paint(
        color,
        "\u{1b}[1;36m",
        &format!("  distribution  GB/s ({} samples)", histogram.sample_count),
    ));
    lines.extend(rows.into_iter().map(|row| format!("    {row}")));
}

fn render_histogram(
    histogram: &Histogram,
    median: Option<f64>,
    color: ColorMode,
) -> Option<Vec<String>> {
    const BAR_WIDTH: usize = 24;

    if color == ColorMode::Never {
        return histogram.render_ascii(BAR_WIDTH, median);
    }

    let rows = histogram.rows(BAR_WIDTH, median)?;
    let count_width = rows
        .iter()
        .map(|row| row.count.to_string().len())
        .max()
        .unwrap_or(1);
    Some(
        rows.into_iter()
            .map(|row| {
                let bar = "█".repeat(row.bar.len());
                let padded = format!("{bar:<BAR_WIDTH$}");
                let painted_bar = if row.marks_median {
                    paint(color, "\u{1b}[1;32m", &padded)
                } else {
                    paint(color, "\u{1b}[36m", &padded)
                };
                let marker = if row.marks_median {
                    format!("  {}", paint(color, "\u{1b}[1;32m", "◆ median"))
                } else {
                    String::new()
                };
                format!(
                    "{} │ {painted_bar} {:>count_width$}{marker}",
                    row.label, row.count
                )
            })
            .collect(),
    )
}

fn case_label(case: &BenchCase) -> String {
    match case.transfer_class {
        TransferClass::H2D => format!("H2D pinned host -> {}", case.destination),
        TransferClass::D2H => format!("D2H {} -> pinned host", case.source),
        TransferClass::D2DSameDevice => {
            format!("D2D same-device {} -> {}", case.source, case.destination)
        }
        TransferClass::D2DDirect => {
            format!("D2D direct {} -> {}", case.source, case.destination)
        }
        TransferClass::D2DStaged => format!(
            "D2D explicit-staged {} -> {} via pinned host",
            case.source, case.destination
        ),
    }
}

fn render_link_inline(link: &LinkInfo) -> String {
    match link {
        LinkInfo::Known {
            generation,
            width,
            theoretical_gb_s,
        } => format!(
            "negotiated PCIe Gen{} x{}, {} theoretical",
            generation,
            width,
            format_rate(*theoretical_gb_s)
        ),
        LinkInfo::Unknown { reason } => format!("PCIe unknown: {reason}"),
    }
}

fn render_negotiated_pcie_percent(rate_gb_s: f64, link: &LinkInfo) -> Option<String> {
    match link {
        LinkInfo::Known {
            theoretical_gb_s, ..
        } if *theoretical_gb_s > 0.0 => Some(format!(
            "{}% of negotiated PCIe theoretical",
            format_percent(rate_gb_s / theoretical_gb_s * 100.0)
        )),
        LinkInfo::Known { .. } | LinkInfo::Unknown { .. } => None,
    }
}

fn render_timing(timing: crate::cli::TimingMode) -> &'static str {
    match timing {
        crate::cli::TimingMode::WallClock => "wall clock",
        crate::cli::TimingMode::DeviceTimestamps => "device timestamps",
    }
}

fn render_engine_flags(flags: QueueFlags) -> &'static str {
    match (flags.copy, flags.compute) {
        (true, true) => "compute+copy",
        (true, false) => "copy",
        (false, true) => "compute",
        (false, false) => "no-copy-no-compute",
    }
}

fn format_rate(value: f64) -> String {
    format!("{} GB/s", format_nonzero_metric(value, 1))
}

fn format_rate_triplet(values: [f64; 3]) -> [String; 3] {
    format_triplet(values, 1, |value, decimals| {
        format!("{} GB/s", format_nonzero_metric(value, decimals))
    })
}

fn format_percent(value: f64) -> String {
    format_nonzero_metric(value, 0)
}

fn format_seconds_triplet(values: [f64; 3]) -> [String; 3] {
    format_triplet(values, 2, format_seconds_with_decimals)
}

fn estimate_table(time: [String; 3], throughput: [String; 3], color: ColorMode) -> [String; 3] {
    const LABEL_WIDTH: usize = 12;
    const VALUE_GAP: &str = "  ";

    let value_width = time
        .iter()
        .chain(&throughput)
        .map(String::len)
        .chain(["lower".len(), "median".len(), "upper".len()])
        .max()
        .unwrap_or(0);
    let cell = |value: &str, style: &str| paint(color, style, &format!("{value:<value_width$}"));
    let heading = format!(
        "{:16}{}{VALUE_GAP}{}{VALUE_GAP}{}",
        "",
        cell("lower", "\u{1b}[2m"),
        cell("median", "\u{1b}[2m"),
        cell("upper", "\u{1b}[2m")
    );
    let row = |label: &str, values: [String; 3]| {
        format!(
            "  {label:<LABEL_WIDTH$}[ {}{VALUE_GAP}{}{VALUE_GAP}{} ]",
            cell(&values[0], "\u{1b}[2m"),
            cell(&values[1], "\u{1b}[1;32m"),
            cell(&values[2], "\u{1b}[2m")
        )
    };

    [heading, row("time", time), row("throughput", throughput)]
}

fn format_seconds_with_decimals(seconds: f64, decimals: usize) -> String {
    if !seconds.is_finite() {
        return "unknown".to_owned();
    }
    if seconds == 0.0 {
        return "0s".to_owned();
    }

    let abs = seconds.abs();
    if abs < 1e-6 {
        format!("{} ns", format_nonzero_metric(seconds * 1e9, decimals))
    } else if abs < 1e-3 {
        format!("{} us", format_nonzero_metric(seconds * 1e6, decimals))
    } else if abs < 1.0 {
        format!("{} ms", format_nonzero_metric(seconds * 1e3, decimals))
    } else {
        format!("{} s", format_nonzero_metric(seconds, decimals))
    }
}

fn format_triplet(
    values: [f64; 3],
    preferred_decimals: usize,
    formatter: impl Fn(f64, usize) -> String,
) -> [String; 3] {
    for decimals in preferred_decimals..=6 {
        let rendered = values.map(|value| formatter(value, decimals));
        let hides_variation = (0..values.len()).any(|left| {
            (left + 1..values.len())
                .any(|right| values[left] != values[right] && rendered[left] == rendered[right])
        });
        if !hides_variation {
            return rendered;
        }
    }

    values.map(|value| formatter(value, 9))
}

fn format_nonzero_metric(value: f64, preferred_decimals: usize) -> String {
    if !value.is_finite() {
        return "unknown".to_owned();
    }
    if value == 0.0 {
        return "0".to_owned();
    }

    for decimals in preferred_decimals..=12 {
        let candidate = format!("{value:.decimals$}");
        if candidate.parse::<f64>().unwrap_or(0.0) != 0.0 {
            return trim_decimal_zeros(&candidate);
        }
    }

    trim_decimal_zeros(&format!("{value:.15}"))
}

fn format_human_decimal(value: f64, max_decimals: usize) -> String {
    trim_decimal_zeros(&format!("{value:.max_decimals$}"))
}

fn trim_decimal_zeros(value: &str) -> String {
    if let Some((whole, fraction)) = value.split_once('.') {
        let trimmed = fraction.trim_end_matches('0');
        if trimmed.is_empty() {
            whole.to_owned()
        } else {
            format!("{whole}.{trimmed}")
        }
    } else {
        value.to_owned()
    }
}

fn sanitize_human_text(value: &str) -> String {
    value
        .replace("queue group ordinal ", "queue group ")
        .replace(" ordinal ", " queue group ")
}

fn paint(color: ColorMode, code: &str, text: &str) -> String {
    match color {
        ColorMode::Ansi => format!("{code}{text}\u{1b}[0m"),
        ColorMode::Never => text.to_owned(),
    }
}

fn finish_lines(lines: Vec<String>) -> String {
    let mut output = lines.join("\n");
    output.push('\n');
    output
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::cli::{TimingMode, TransferClass};
    use crate::output::{
        AllocationKind, BenchCase, BenchReport, CaseOutcome, ColorMode, DeviceInfo, Endpoint,
        LinkInfo, ListReport, Operation, PeerAccess, PeerAccessInfo, QueueFlags, QueueGroupInfo,
        TextOptions,
    };
    use crate::stats;

    use super::*;

    fn measured_case_with_samples(samples: Vec<f64>) -> BenchCase {
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
                time_summary: Box::new(summary),
                summary,
                samples_gb_s: samples,
            },
        }
    }

    fn measured_case() -> BenchCase {
        measured_case_with_samples(vec![49.8, 50.7, 51.2, 51.6, 51.9])
    }

    #[test]
    fn renders_measured_text_with_benchmark_details_and_no_ordinal() {
        let report = BenchReport {
            cases: vec![measured_case()],
        };
        let output = render_bench_text(&report, &TextOptions::default());

        assert!(output.contains("D2H dev0 -> pinned host"));
        assert!(output.contains("256 MiB on copy engine 1  [wall clock, 5 samples, 1 s warm-up]"));
        assert!(output.contains("negotiated PCIe Gen5 x16, 63 GB/s theoretical"));
        assert!(output.contains("time"));
        assert!(output.contains("throughput"));
        assert!(output.contains("81% of negotiated PCIe theoretical"));
        assert!(output.contains("MAD"));
        assert!(output.contains("outliers"));
        assert!(output.contains("median"));
        assert!(!output.contains("ordinal"));
        assert!(!output.contains("\u{1b}["));
    }

    #[test]
    fn displays_time_confidence_interval_from_duration_statistics() {
        let case = measured_case_with_samples(vec![10.0, 20.0, 40.0]);
        let text = render_case_text(
            &case,
            &TextOptions {
                include_histogram: false,
                color: ColorMode::Never,
            },
        );

        assert!(text.contains("time        ["));
        assert!(text.contains("20 s"));
        assert!(text.contains("throughput  ["));
        assert!(text.contains("20 GB/s"));
        assert!(text.contains("95% bootstrap confidence interval (10000 resamples)"));
        assert!(text.contains("sample       p5 11 GB/s, p95 38 GB/s"));
    }

    #[test]
    fn nonzero_small_mad_does_not_round_to_zero() {
        let case = measured_case_with_samples(vec![1.000_000, 1.000_001, 1.000_002]);
        let text = render_case_text(
            &case,
            &TextOptions {
                include_histogram: false,
                color: ColorMode::Never,
            },
        );

        assert!(text.contains("MAD 0.000001 GB/s"));
        assert!(!text.contains("MAD 0 GB/s"));
    }

    #[test]
    fn exact_zero_mad_renders_as_zero() {
        let case = measured_case_with_samples(vec![7.0, 7.0, 7.0]);
        let text = render_case_text(
            &case,
            &TextOptions {
                include_histogram: false,
                color: ColorMode::Never,
            },
        );

        assert!(text.contains("MAD 0 GB/s (no variation at timer resolution)"));
    }

    #[test]
    fn close_quantiles_use_enough_precision_to_remain_distinct() {
        let rendered = format_rate_triplet([13.701, 13.704, 13.709]);

        assert_eq!(
            rendered,
            [
                "13.701 GB/s".to_owned(),
                "13.704 GB/s".to_owned(),
                "13.709 GB/s".to_owned()
            ]
        );
    }

    #[test]
    fn estimate_table_uses_shared_value_columns() {
        let [heading, time, throughput] = estimate_table(
            [
                "19.419 ms".to_owned(),
                "19.425 ms".to_owned(),
                "19.429 ms".to_owned(),
            ],
            [
                "13.816 GB/s".to_owned(),
                "13.819 GB/s".to_owned(),
                "13.823 GB/s".to_owned(),
            ],
            ColorMode::Never,
        );

        assert_eq!(heading.find("lower"), time.find("19.419"));
        assert_eq!(heading.find("lower"), throughput.find("13.816"));
        assert_eq!(heading.find("median"), time.find("19.425"));
        assert_eq!(heading.find("median"), throughput.find("13.819"));
        assert_eq!(heading.find("upper"), time.find("19.429"));
        assert_eq!(heading.find("upper"), throughput.find("13.823"));
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
    fn ansi_histogram_uses_unicode_bars_and_median_marker() {
        let output = render_case_text(
            &measured_case(),
            &TextOptions {
                include_histogram: true,
                color: ColorMode::Ansi,
            },
        );

        assert!(output.contains("distribution  GB/s"));
        assert!(output.contains('█'));
        assert!(output.contains('│'));
        assert!(output.contains("◆ median"));
    }

    #[test]
    fn skipped_direct_text_separates_observation_peer_access_and_path_inference() {
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
                    reason:
                        "destination dev1 queue group ordinal 0 does not advertise copy capability"
                            .to_owned(),
                },
            }],
        };

        let text = render_bench_text(&report, &TextOptions::default());
        assert!(text.contains("D2D direct dev0 -> dev1"));
        assert!(text.contains("compute+copy engine 0"));
        assert!(text.contains("direct Level Zero copy"));
        assert!(text.contains("peer access no"));
        assert!(text.contains("physical route not inferred"));
        assert!(text.contains("queue group 0 does not advertise copy capability"));
        assert!(!text.contains("ordinal"));
    }

    #[test]
    fn renders_list_text_with_engine_names() {
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
        assert!(text.contains("engine 1 (copy)"));
        assert!(text.contains("dev0 -> dev1  yes"));
        assert!(!text.contains("ordinal"));
    }

    #[test]
    fn status_label_is_compact_and_avoids_queue_terms() {
        assert_eq!(
            status_label_from_case_id("d2d-direct/dev0-to-dev1/256MiB/engine-2/wall-clock"),
            "D2D direct dev0 -> dev1 / engine 2"
        );
    }
}
