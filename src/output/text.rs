use super::model::{
    BenchCase, BenchReport, CaseOutcome, ColorMode, LinkInfo, ListReport, Operation, PeerAccess,
    PeerRoute, QueueFlags, QueueStreamInfo, SystemInfo, TextOptions,
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
                "  queue group {} ({}, {})",
                queue.ordinal,
                render_queue_flags(queue.flags),
                render_queue_count(queue.queue_count)
            ));
        }
    }

    if !report.peer_access.is_empty() {
        lines.push(String::new());
        lines.push("device-to-device access (Level Zero peer check)".to_owned());
        for peer in &report.peer_access {
            let reason = match &peer.access {
                PeerAccess::Unknown(reason) => format!(" ({reason})"),
                PeerAccess::Yes | PeerAccess::No => String::new(),
            };
            lines.push(format!(
                "  dev{} -> dev{}  {}{}  [{}]",
                peer.from_device,
                peer.to_device,
                peer.access.as_field(),
                reason,
                render_peer_route(&peer.route)
            ));
        }
    }

    finish_lines(&lines)
}

pub fn render_bench_text(report: &BenchReport, options: &TextOptions) -> String {
    if options.summary_only {
        return render_bench_summary(report, options);
    }

    let mut lines = render_system_lines(&report.system, options.color);

    if report.cases.is_empty() {
        lines.push("no benchmark cases selected".to_owned());
        return finish_lines(&lines);
    }

    for (case_index, case) in report.cases.iter().enumerate() {
        if case_index > 0 {
            lines.push(String::new());
        }
        lines.extend(render_case_lines(case, options));
    }
    if report.cases.len() > 1 {
        lines.push(String::new());
        lines.extend(render_summary_lines(&report.cases, options.color));
    }

    finish_lines(&lines)
}

pub fn render_bench_summary(report: &BenchReport, options: &TextOptions) -> String {
    let mut lines = render_system_lines(&report.system, options.color);
    lines.extend(render_summary_lines(&report.cases, options.color));
    finish_lines(&lines)
}

pub(crate) fn render_summary_text(cases: &[BenchCase], options: &TextOptions) -> String {
    finish_lines(&render_summary_lines(cases, options.color))
}

pub(crate) fn render_system_text(system: &SystemInfo, color: ColorMode) -> String {
    finish_lines(&render_system_lines(system, color))
}

pub fn render_case_text(case: &BenchCase, options: &TextOptions) -> String {
    finish_lines(&render_case_lines(case, options))
}

fn render_system_lines(system: &SystemInfo, color: ColorMode) -> Vec<String> {
    let mut lines = vec![
        paint(color, "\u{1b}[1;36m", "System under test"),
        format!("  Host  {}", system.host.cpu_model),
        format!("        {}", render_cpu_topology(system)),
    ];

    for device in &system.devices {
        let pci = device.pci_address.as_deref().unwrap_or("unknown");
        lines.push(format!("  dev{}  {}", device.index, device.name));
        lines.push(format!(
            "        PCI {pci} | {}",
            render_system_link(&device.pcie_link)
        ));
        lines.push(format!(
            "        queue groups {}",
            render_system_queue_groups(device)
        ));
    }
    lines.push(String::new());
    lines
}

fn render_cpu_topology(system: &SystemInfo) -> String {
    let threads = match system.host.logical_cpus {
        0 => "threads unknown".to_owned(),
        count => format!("{count} {}", plural(count, "thread", "threads")),
    };
    match (system.host.sockets, system.host.physical_cores) {
        (Some(sockets), Some(cores)) => format!(
            "{sockets} {}, {cores} {}, {threads}",
            plural(sockets, "socket", "sockets"),
            plural(cores, "core", "cores")
        ),
        (Some(sockets), None) => {
            format!(
                "{sockets} {}, {threads}",
                plural(sockets, "socket", "sockets")
            )
        }
        (None, Some(cores)) => {
            format!("{cores} {}, {threads}", plural(cores, "core", "cores"))
        }
        (None, None) => threads,
    }
}

fn render_system_link(link: &LinkInfo) -> String {
    match link {
        LinkInfo::Known {
            generation,
            width,
            theoretical_gb_s,
        } => format!(
            "Gen{generation} x{width} | {} theoretical",
            format_rate(*theoretical_gb_s)
        ),
        LinkInfo::Unknown { reason } => {
            format!("PCIe link unknown ({})", sanitize_human_text(reason))
        }
    }
}

fn render_system_queue_groups(device: &super::model::DeviceInfo) -> String {
    if device.queue_groups.is_empty() {
        return "none".to_owned();
    }

    device
        .queue_groups
        .iter()
        .map(|group| {
            let queues = if group.queue_count > 1 {
                format!(" ({} queues)", group.queue_count)
            } else {
                String::new()
            };
            format!(
                "{} {}{queues}",
                group.ordinal,
                render_queue_flags(group.flags)
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

struct SummaryRow {
    transfer: String,
    path: String,
    queues: String,
    median: String,
    spread: String,
    mad: String,
    outliers: String,
    mild_outliers: usize,
    severe_outliers: usize,
    skipped: bool,
}

const SUMMARY_HEADERS: [&str; 7] = [
    "transfer",
    "path",
    "queue group(s)",
    "median GB/s",
    "p5..p95 GB/s",
    "MAD GB/s",
    "outliers",
];
const P2P_FACT_HEADERS: [&str; 4] = ["pair", "copy request", "peer access", "PCIe topology"];

fn render_summary_lines(cases: &[BenchCase], color: ColorMode) -> Vec<String> {
    if cases.is_empty() {
        return vec!["no benchmark cases selected".to_owned()];
    }

    let mut lines = summary_heading(cases, color);
    let rows = cases.iter().map(summary_row).collect::<Vec<_>>();
    lines.extend(render_summary_table(&rows, color));
    append_skipped_cases(&mut lines, cases, color);
    lines.extend(render_p2p_facts(cases, color));
    append_summary_notes(&mut lines, cases, color);
    lines
}

fn summary_heading(cases: &[BenchCase], color: ColorMode) -> Vec<String> {
    let measured = cases
        .iter()
        .filter(|case| matches!(case.outcome, CaseOutcome::Measured { .. }))
        .count();
    let skipped = cases.len() - measured;
    let first = &cases[0];
    let case_label = if cases.len() == 1 { "case" } else { "cases" };
    vec![
        paint(color, "\u{1b}[1;36m", "Run summary"),
        paint(
            color,
            "\u{1b}[2m",
            &format!(
                "  {} {case_label} | {measured} measured | {skipped} skipped | {} | {} | {} samples | {} warm-up | {}",
                cases.len(),
                format_bytes(first.byte_count),
                render_timing(first.timing),
                first.requested_samples,
                format_duration(first.warmup),
                first.mode
            ),
        ),
        String::new(),
    ]
}

fn render_summary_table(rows: &[SummaryRow], color: ColorMode) -> Vec<String> {
    let widths = std::array::from_fn::<_, 7, _>(|index| {
        rows.iter()
            .map(|row| summary_row_fields(row)[index].len())
            .chain(std::iter::once(SUMMARY_HEADERS[index].len()))
            .max()
            .unwrap_or(SUMMARY_HEADERS[index].len())
    });
    let header = SUMMARY_HEADERS
        .iter()
        .enumerate()
        .map(|(index, value)| format!("{value:<width$}", width = widths[index]))
        .collect::<Vec<_>>()
        .join("  ");
    let mut lines = vec![format!("  {}", paint(color, "\u{1b}[2m", &header))];
    lines.extend(
        rows.iter()
            .map(|row| render_summary_row(row, widths, color)),
    );
    lines
}

fn render_summary_row(row: &SummaryRow, widths: [usize; 7], color: ColorMode) -> String {
    let fields = summary_row_fields(row);
    let transfer = format!("{:<width$}", fields[0], width = widths[0]);
    let path = format!("{:<width$}", fields[1], width = widths[1]);
    let queues = format!("{:<width$}", fields[2], width = widths[2]);
    let median = format!("{:>width$}", fields[3], width = widths[3]);
    let spread = format!("{:>width$}", fields[4], width = widths[4]);
    let mad = format!("{:>width$}", fields[5], width = widths[5]);
    let outliers = format!("{:>width$}", fields[6], width = widths[6]);
    let metric_style = if row.skipped {
        "\u{1b}[33m"
    } else {
        "\u{1b}[1;32m"
    };
    let outlier_style = if row.severe_outliers > 0 {
        "\u{1b}[31m"
    } else if row.mild_outliers > 0 {
        "\u{1b}[33m"
    } else {
        "\u{1b}[2m"
    };
    format!(
        "  {transfer}  {}  {queues}  {}  {spread}  {mad}  {}",
        paint(color, "\u{1b}[36m", &path),
        paint(color, metric_style, &median),
        paint(color, outlier_style, &outliers)
    )
}

fn append_skipped_cases(lines: &mut Vec<String>, cases: &[BenchCase], color: ColorMode) {
    let skipped_cases = cases
        .iter()
        .filter_map(|case| match &case.outcome {
            CaseOutcome::Skipped { reason } => Some((case, reason)),
            CaseOutcome::Measured { .. } => None,
        })
        .collect::<Vec<_>>();
    if !skipped_cases.is_empty() {
        lines.push(String::new());
        lines.push(paint(color, "\u{1b}[1;33m", "  Skipped cases"));
        for (case, reason) in skipped_cases {
            lines.push(format!(
                "    {}: {}",
                case_label(case),
                sanitize_human_text(reason)
            ));
        }
    }
}

fn render_p2p_facts(cases: &[BenchCase], color: ColorMode) -> Vec<String> {
    let mut facts = Vec::<[String; 4]>::new();
    for case in cases {
        let Operation::Direct { peer_access, route } = &case.operation else {
            continue;
        };
        let pair = format!("{} -> {}", case.source, case.destination);
        if facts.iter().any(|fact| fact[0] == pair) {
            continue;
        }
        facts.push([
            pair,
            "Level Zero direct".to_owned(),
            peer_access.as_field().to_owned(),
            render_peer_route_short(route).to_owned(),
        ]);
    }
    if facts.is_empty() {
        return Vec::new();
    }

    let widths = std::array::from_fn::<_, 4, _>(|index| {
        facts
            .iter()
            .map(|fact| fact[index].len())
            .chain(std::iter::once(P2P_FACT_HEADERS[index].len()))
            .max()
            .unwrap_or(P2P_FACT_HEADERS[index].len())
    });
    let header = P2P_FACT_HEADERS
        .iter()
        .enumerate()
        .map(|(index, value)| format!("{value:<width$}", width = widths[index]))
        .collect::<Vec<_>>()
        .join("  ")
        .trim_end()
        .to_owned();
    let mut lines = vec![
        String::new(),
        paint(color, "\u{1b}[1;36m", "P2P path facts"),
        format!("  {}", paint(color, "\u{1b}[2m", &header)),
    ];
    for fact in facts {
        let topology = format!("{:<width$}", fact[3], width = widths[3]);
        lines.push(format!(
            "  {:<w0$}  {:<w1$}  {:<w2$}  {}",
            fact[0],
            fact[1],
            fact[2],
            paint(color, "\u{1b}[36m", &topology),
            w0 = widths[0],
            w1 = widths[1],
            w2 = widths[2]
        ));
    }
    lines
}

fn append_summary_notes(lines: &mut Vec<String>, cases: &[BenchCase], color: ColorMode) {
    let has_direct = cases
        .iter()
        .any(|case| matches!(case.operation, Operation::Direct { .. }));
    let has_staged = cases
        .iter()
        .any(|case| matches!(case.operation, Operation::ExplicitStaged { .. }));
    if has_direct {
        lines.push(String::new());
        lines.push(paint(
            color,
            "\u{1b}[2m",
            "  direct: one Level Zero copy request; peer access is the API capability result",
        ));
    }
    if has_staged {
        lines.push(paint(
            color,
            "\u{1b}[2m",
            "  staged: D2H, synchronize, then H2D; payload rate covers both legs, copy-traffic rate is 2x",
        ));
    }
    if has_direct {
        lines.push(paint(
            color,
            "\u{1b}[2m",
            "  TP roofline: use the median as an isolated directional pair ceiling; collectives may be lower",
        ));
    }
}

fn summary_row(case: &BenchCase) -> SummaryRow {
    let (median, spread, mad, outliers, mild_outliers, severe_outliers, skipped) =
        match &case.outcome {
            CaseOutcome::Measured {
                time_summary,
                summary,
                ..
            } => {
                let [p5, median_with_unit, p95] =
                    format_rate_triplet([summary.p5, summary.median, summary.p95]);
                let median = if matches!(case.operation, Operation::ExplicitStaged { .. }) {
                    format!(
                        "{} payload / {} copy traffic",
                        rate_number(&median_with_unit),
                        format_nonzero_metric(summary.median * 2.0, 1)
                    )
                } else {
                    rate_number(&median_with_unit).to_owned()
                };
                let mild = time_summary.outliers.counts.mild;
                let severe = time_summary.outliers.counts.severe;
                (
                    median,
                    format!("{}..{}", rate_number(&p5), rate_number(&p95)),
                    format_nonzero_metric(summary.mad, 1),
                    format!("{}/{}", mild + severe, time_summary.count),
                    mild,
                    severe,
                    false,
                )
            }
            CaseOutcome::Skipped { .. } => (
                "SKIPPED".to_owned(),
                "-".to_owned(),
                "-".to_owned(),
                "-".to_owned(),
                0,
                0,
                true,
            ),
        };

    SummaryRow {
        transfer: summary_transfer(case),
        path: summary_path(case),
        queues: summary_queues(case),
        median,
        spread,
        mad,
        outliers,
        mild_outliers,
        severe_outliers,
        skipped,
    }
}

fn summary_row_fields(row: &SummaryRow) -> [&str; 7] {
    [
        &row.transfer,
        &row.path,
        &row.queues,
        &row.median,
        &row.spread,
        &row.mad,
        &row.outliers,
    ]
}

fn summary_transfer(case: &BenchCase) -> String {
    match case.transfer_class {
        TransferClass::H2D => format!("H2D {} -> {}", case.source, case.destination),
        TransferClass::D2H => format!("D2H {} -> {}", case.source, case.destination),
        TransferClass::D2DSameDevice => format!("D2D same {}", case.source),
        TransferClass::D2DDirect => {
            format!("D2D direct {} -> {}", case.source, case.destination)
        }
        TransferClass::D2DStaged => {
            format!("D2D staged {} -> {}", case.source, case.destination)
        }
    }
}

fn summary_path(case: &BenchCase) -> String {
    match &case.operation {
        Operation::HostToDevice | Operation::DeviceToHost => "pinned host".to_owned(),
        Operation::SameDevice => "device memory".to_owned(),
        Operation::Direct { .. } => "direct copy".to_owned(),
        Operation::ExplicitStaged { .. } => "pinned host (2 legs)".to_owned(),
    }
}

fn summary_queues(case: &BenchCase) -> String {
    let first = if let Some(group) = &case.selected_group {
        format!("{} ({})", group.ordinal, render_queue_flags(group.flags))
    } else if case.streams.is_empty() {
        "-".to_owned()
    } else {
        render_streams_compact(&case.streams)
    };

    if case.second_phase_streams.is_empty() {
        first
    } else {
        format!(
            "{first} / {}",
            render_streams_compact(&case.second_phase_streams)
        )
    }
}

fn rate_number(rate: &str) -> &str {
    rate.strip_suffix(" GB/s").unwrap_or(rate)
}

pub(crate) fn status_label_from_case_id(id: &str) -> String {
    let mut parts = id.split('/');
    let class = parts.next().unwrap_or_default();
    let endpoints = parts.next().unwrap_or_default();
    let _size = parts.next();
    let queue_scope = parts
        .next()
        .map(|part| match part.strip_prefix("group-") {
            Some(id) => format!(" / queue group {id}"),
            None if part == "all-copy-groups" => " / all copy queue groups".to_owned(),
            None => String::new(),
        })
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
        format!("{class}{queue_scope}")
    } else {
        format!("{class} {endpoints}{queue_scope}")
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
    let mut lines = vec![paint(options.color, "\u{1b}[1m", &case_label(case))];
    lines.push(paint(options.color, "\u{1b}[1;36m", "  Transfer"));
    append_transfer_details(&mut lines, case);
    lines.push(String::new());
    let path_heading = if matches!(
        case.operation,
        Operation::Direct { .. } | Operation::ExplicitStaged { .. }
    ) {
        "  P2P evidence"
    } else {
        "  Copy path"
    };
    lines.push(paint(options.color, "\u{1b}[1;36m", path_heading));
    append_path_evidence(&mut lines, case);

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

fn append_transfer_details(lines: &mut Vec<String>, case: &BenchCase) {
    push_detail(lines, "payload", format_bytes(case.byte_count));
    let mode = match case.mode {
        crate::cli::BenchMode::Single => "single queue".to_owned(),
        crate::cli::BenchMode::Saturation => format!(
            "saturation across {} {}; payload partitioned across queues",
            case.streams.len(),
            plural(case.streams.len(), "queue", "queues")
        ),
    };
    push_detail(lines, "mode", mode);
    let queue_label = if case.second_phase_streams.is_empty() {
        "queues"
    } else {
        "source queues"
    };
    push_detail(lines, queue_label, render_streams(&case.streams));
    if !case.second_phase_streams.is_empty() {
        push_detail(
            lines,
            "destination queues",
            format!(
                "{}; starts after source queues finish",
                render_streams(&case.second_phase_streams)
            ),
        );
    }
    push_detail(lines, "memory", render_allocation(case.allocation));
    push_detail(
        lines,
        "timing",
        format!(
            "{}, {} samples, {} warm-up",
            render_timing(case.timing),
            case.requested_samples,
            format_duration(case.warmup)
        ),
    );
    if submitted_copy_bytes(case) != case.byte_count {
        push_detail(
            lines,
            "copy traffic",
            format!(
                "{} submitted for {} logical payload",
                format_bytes(submitted_copy_bytes(case)),
                format_bytes(case.byte_count)
            ),
        );
    }
}

fn append_path_evidence(lines: &mut Vec<String>, case: &BenchCase) {
    match &case.operation {
        Operation::Direct { peer_access, route } => {
            push_detail(lines, "copy request", "direct GPU-memory copy (Level Zero)");
            push_detail(
                lines,
                "peer access",
                format!(
                    "{} (zeDeviceCanAccessPeer = {})",
                    render_peer_support(peer_access),
                    peer_access.as_field(),
                ),
            );
            push_detail(lines, "PCIe topology", render_peer_route(route));
            push_detail(lines, "host staging", "none requested by xfer");
        }
        Operation::ExplicitStaged { route } => {
            push_detail(lines, "copy request", "GPU -> pinned host -> GPU");
            push_detail(lines, "synchronization", "wait between D2H and H2D legs");
            push_detail(
                lines,
                "PCIe topology",
                format!("{}; direct peer route not used", render_peer_route(route)),
            );
            push_detail(lines, "host staging", "yes, explicitly requested by xfer");
        }
        Operation::HostToDevice => {
            push_detail(lines, "copy request", "pinned host -> GPU memory");
        }
        Operation::DeviceToHost => {
            push_detail(lines, "copy request", "GPU memory -> pinned host");
        }
        Operation::SameDevice => {
            push_detail(lines, "copy request", "copy between allocations on one GPU");
        }
    }
}

fn push_detail(lines: &mut Vec<String>, label: &str, value: impl std::fmt::Display) {
    const LABEL_WIDTH: usize = 20;
    lines.push(format!("    {label:<LABEL_WIDTH$}{value}"));
}

fn render_peer_support(access: &PeerAccess) -> &'static str {
    match access {
        PeerAccess::Yes => "supported",
        PeerAccess::No => "not supported",
        PeerAccess::Unknown(_) => "unknown",
    }
}

fn render_allocation(allocation: super::model::AllocationKind) -> &'static str {
    match allocation {
        super::model::AllocationKind::PinnedHost => "pinned host + device memory",
        super::model::AllocationKind::Device => "device memory",
        super::model::AllocationKind::PinnedStaging => "device memory + pinned host staging",
    }
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
    let rate_label = if matches!(case.operation, Operation::ExplicitStaged { .. }) {
        "payload"
    } else {
        "throughput"
    };
    let [heading, time, throughput] = estimate_table(time, throughput, rate_label, options.color);
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
    if matches!(case.operation, Operation::ExplicitStaged { .. }) {
        lines.push(format!(
            "  copy traffic median {} GB/s (2 submitted bytes per logical payload byte)",
            format_nonzero_metric(summary.median * 2.0, 1)
        ));
    }

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

fn render_peer_route(route: &PeerRoute) -> String {
    match route {
        PeerRoute::SameRootPort { root_port } => {
            format!("same root port {root_port}")
        }
        PeerRoute::SharedUpstreamBridge { common_bridge } => {
            format!("shared upstream bridge {common_bridge}")
        }
        PeerRoute::DifferentRootPorts {
            host_bridge,
            source_root_port,
            destination_root_port,
        } => format!(
            "different root ports {source_root_port} -> {destination_root_port} on {host_bridge}"
        ),
        PeerRoute::CrossHostBridges {
            source_host_bridge,
            destination_host_bridge,
        } => format!("different host bridges {source_host_bridge} -> {destination_host_bridge}"),
        PeerRoute::Unknown(reason) => format!("topology unknown: {reason}"),
    }
}

fn render_peer_route_short(route: &PeerRoute) -> &'static str {
    match route {
        PeerRoute::SameRootPort { .. } => "same root port",
        PeerRoute::SharedUpstreamBridge { .. } => "shared upstream",
        PeerRoute::DifferentRootPorts { .. } => "different root ports",
        PeerRoute::CrossHostBridges { .. } => "different host bridges",
        PeerRoute::Unknown(_) => "unknown",
    }
}

fn render_queue_flags(flags: QueueFlags) -> &'static str {
    match (flags.copy, flags.compute) {
        (true, true) => "compute+copy",
        (true, false) => "copy",
        (false, true) => "compute",
        (false, false) => "no-copy-no-compute",
    }
}

fn render_queue_count(count: u32) -> String {
    if count == 1 {
        "1 queue".to_owned()
    } else {
        format!("{count} queues")
    }
}

fn render_streams(streams: &[QueueStreamInfo]) -> String {
    if streams.is_empty() {
        return "-".to_owned();
    }

    streams
        .iter()
        .map(|stream| {
            format!(
                "queue group {} / queue {} ({})",
                stream.group_ordinal,
                stream.queue_index,
                render_queue_flags(stream.flags)
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn render_streams_compact(streams: &[QueueStreamInfo]) -> String {
    let mut groups = Vec::<(u32, usize)>::new();
    for stream in streams {
        if let Some((_, count)) = groups
            .iter_mut()
            .find(|(group, _)| *group == stream.group_ordinal)
        {
            *count += 1;
        } else {
            groups.push((stream.group_ordinal, 1));
        }
    }

    groups
        .into_iter()
        .map(|(group, count)| {
            if count == 1 {
                group.to_string()
            } else {
                format!("{group} ({count} queues)")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn submitted_copy_bytes(case: &BenchCase) -> u64 {
    if matches!(case.operation, Operation::ExplicitStaged { .. }) {
        case.byte_count.saturating_mul(2)
    } else {
        case.byte_count
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

fn estimate_table(
    time: [String; 3],
    throughput: [String; 3],
    rate_label: &str,
    color: ColorMode,
) -> [String; 3] {
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

    [heading, row("time", time), row(rate_label, throughput)]
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
        .replace("queue-group ordinals", "queue group IDs")
        .replace(" ordinal ", " ID ")
}

fn plural<'word>(count: usize, singular: &'word str, plural: &'word str) -> &'word str {
    if count == 1 { singular } else { plural }
}

fn paint(color: ColorMode, code: &str, text: &str) -> String {
    match color {
        ColorMode::Ansi => format!("{code}{text}\u{1b}[0m"),
        ColorMode::Never => text.to_owned(),
    }
}

fn finish_lines(lines: &[String]) -> String {
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
        LinkInfo, ListReport, Operation, PeerAccess, PeerAccessInfo, PeerRoute, QueueFlags,
        QueueGroupInfo, TextOptions,
    };
    use crate::stats;

    use super::*;

    fn measured_case_with_samples(samples: Vec<f64>) -> BenchCase {
        let summary = stats::summarize(&samples).expect("summary");

        BenchCase {
            mode: crate::cli::BenchMode::Single,
            selected_group: Some(QueueGroupInfo {
                ordinal: 1,
                flags: QueueFlags {
                    copy: true,
                    compute: false,
                },
                queue_count: 1,
            }),
            streams: vec![crate::output::QueueStreamInfo {
                group_ordinal: 1,
                queue_index: 0,
                flags: QueueFlags {
                    copy: true,
                    compute: false,
                },
            }],
            second_phase_streams: Vec::new(),
            verification_stream: None,
            transfer_class: TransferClass::D2H,
            operation: Operation::DeviceToHost,
            source: Endpoint::Device(0),
            destination: Endpoint::Host,
            byte_count: 256 * 1024 * 1024,
            allocation: AllocationKind::PinnedHost,
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
            system: crate::output::test_system(1),
            cases: vec![measured_case()],
        };
        let output = render_bench_text(&report, &TextOptions::default());

        assert!(output.starts_with("System under test\n"));
        assert_eq!(output.matches("System under test").count(), 1);
        assert!(output.contains("Host  Test CPU"));
        assert!(output.contains("dev0  Test GPU"));
        assert!(output.contains("PCI 0000:01:00.0 | Gen5 x16 | 63 GB/s theoretical"));
        assert!(output.contains("D2H dev0 -> pinned host"));
        assert!(output.contains("Transfer"));
        assert!(output.contains("payload             256 MiB"));
        assert!(output.contains("queues              queue group 1 / queue 0 (copy)"));
        assert!(output.contains("timing              wall clock, 5 samples, 1 s warm-up"));
        assert!(output.contains("Copy path"));
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
                summary_only: false,
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
                summary_only: false,
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
                summary_only: false,
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
            "throughput",
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
            system: crate::output::test_system(1),
            cases: vec![measured_case()],
        };
        let output = render_bench_text(
            &report,
            &TextOptions {
                include_histogram: false,
                summary_only: false,
                color: ColorMode::Never,
            },
        );

        assert!(!output.contains(" | #"));
    }

    #[test]
    fn summary_only_distinguishes_direct_and_explicit_staged_paths() {
        let mut direct = measured_case();
        direct.transfer_class = TransferClass::D2DDirect;
        direct.operation = Operation::Direct {
            peer_access: PeerAccess::Yes,
            route: PeerRoute::CrossHostBridges {
                source_host_bridge: "pci0000:0a".to_owned(),
                destination_host_bridge: "pci0000:61".to_owned(),
            },
        };
        direct.source = Endpoint::Device(0);
        direct.destination = Endpoint::Device(1);
        direct.allocation = AllocationKind::Device;
        direct.pcie_link = LinkInfo::Unknown {
            reason: "cross-device".to_owned(),
        };

        let mut staged = direct.clone();
        staged.transfer_class = TransferClass::D2DStaged;
        staged.operation = Operation::ExplicitStaged {
            route: PeerRoute::CrossHostBridges {
                source_host_bridge: "pci0000:0a".to_owned(),
                destination_host_bridge: "pci0000:61".to_owned(),
            },
        };
        staged.allocation = AllocationKind::PinnedStaging;
        staged.second_phase_streams = staged.streams.clone();

        let output = render_bench_text(
            &BenchReport {
                system: crate::output::test_system(2),
                cases: vec![direct, staged],
            },
            &TextOptions {
                include_histogram: true,
                summary_only: true,
                color: ColorMode::Never,
            },
        );

        assert!(output.starts_with("System under test\n"));
        assert_eq!(output.matches("System under test").count(), 1);
        assert!(output.contains("Run summary"));
        assert!(output.contains("D2D direct dev0 -> dev1"));
        assert!(output.contains("P2P path facts"));
        assert!(output.contains("Level Zero direct"));
        assert!(!output.contains("physical P2P"));
        assert!(!output.contains("unverified"));
        assert!(output.contains("D2D staged dev0 -> dev1"));
        assert!(output.contains("pinned host"));
        assert!(output.contains("51.2 payload / 102.4 copy traffic"));
        assert!(output.contains("direct: one Level Zero copy request"));
        assert!(output.contains("staged: D2H, synchronize, then H2D"));
        assert!(!output.contains("distribution"));
        assert!(!output.contains("\u{1b}["));
    }

    #[test]
    fn multi_case_detailed_text_ends_with_summary() {
        let report = BenchReport {
            system: crate::output::test_system(1),
            cases: vec![measured_case(), measured_case()],
        };
        let output = render_bench_text(
            &report,
            &TextOptions {
                include_histogram: false,
                summary_only: false,
                color: ColorMode::Never,
            },
        );

        assert_eq!(output.matches("Run summary").count(), 1);
        assert_eq!(output.matches("System under test").count(), 1);
        assert_eq!(output.matches("Transfer").count(), 2);
        assert!(!output.contains("config      mode="));
        assert!(output.rfind("Run summary") > output.rfind("D2H dev0 -> pinned host"));
    }

    #[test]
    fn ansi_histogram_uses_unicode_bars_and_median_marker() {
        let output = render_case_text(
            &measured_case(),
            &TextOptions {
                include_histogram: true,
                summary_only: false,
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
            system: crate::output::test_system(2),
            cases: vec![BenchCase {
                mode: crate::cli::BenchMode::Single,
                selected_group: Some(QueueGroupInfo {
                    ordinal: 0,
                    flags: QueueFlags {
                        copy: true,
                        compute: true,
                    },
                    queue_count: 1,
                }),
                streams: vec![crate::output::QueueStreamInfo {
                    group_ordinal: 0,
                    queue_index: 0,
                    flags: QueueFlags {
                        copy: true,
                        compute: true,
                    },
                }],
                second_phase_streams: Vec::new(),
                verification_stream: Some(crate::output::QueueStreamInfo {
                    group_ordinal: 1,
                    queue_index: 0,
                    flags: QueueFlags {
                        copy: true,
                        compute: false,
                    },
                }),
                transfer_class: TransferClass::D2DDirect,
                operation: Operation::Direct {
                    peer_access: PeerAccess::No,
                    route: PeerRoute::DifferentRootPorts {
                        host_bridge: "pci0000:00".to_owned(),
                        source_root_port: "0000:00:01.0".to_owned(),
                        destination_root_port: "0000:00:02.0".to_owned(),
                    },
                },
                source: Endpoint::Device(0),
                destination: Endpoint::Device(1),
                byte_count: 1024,
                allocation: AllocationKind::Device,
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
        assert!(text.contains("queue group 0 / queue 0 (compute+copy)"));
        assert!(text.contains("direct GPU-memory copy (Level Zero)"));
        assert!(text.contains("not supported (zeDeviceCanAccessPeer = no)"));
        assert!(text.contains("different root ports"));
        assert!(text.contains("host staging        none requested by xfer"));
        assert!(!text.contains("physical P2P"));
        assert!(!text.contains("verification        "));
        assert!(text.contains("queue group 0 does not advertise copy capability"));
        assert!(!text.contains("ordinal"));
    }

    #[test]
    fn renders_list_text_with_queue_group_names() {
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
                    queue_count: 1,
                }],
            }],
            peer_access: vec![PeerAccessInfo {
                from_device: 0,
                to_device: 1,
                access: PeerAccess::Yes,
                route: PeerRoute::SharedUpstreamBridge {
                    common_bridge: "0000:02:00.0".to_owned(),
                },
            }],
        };

        let text = render_list(&report);
        assert!(text.contains("dev0  Intel GPU"));
        assert!(text.contains("queue group 1 (copy, 1 queue)"));
        assert!(text.contains("dev0 -> dev1  yes"));
        assert!(!text.contains("ordinal"));
    }

    #[test]
    fn status_label_uses_queue_group_term() {
        assert_eq!(
            status_label_from_case_id(
                "d2d-direct/dev0-to-dev1/256MiB/group-2/wall-clock/single-streams-1"
            ),
            "D2D direct dev0 -> dev1 / queue group 2"
        );
    }
}
