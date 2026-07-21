use crate::diagnostics::{
    CounterPhase, CounterPhaseReport, CounterRunReport, CpuEvidence, DiagnosticVerdict,
    EvidenceAvailability, EvidenceFailure, EvidenceFailureKind, EvidenceReason, LikelyMechanism,
    P2pDiagnosticReport, PeerRouteQualifier,
};
use crate::evidence::intel_perfmon::EventRole;
use crate::evidence::{
    AcsBridgeOutcome, AcsFlags, AcsMalformedConfig, AcsReadFailure, CpuIdentity, CpuProfile,
};
use crate::histogram::Histogram;
use crate::output::csv_escape;
use crate::output::text::{format_bytes, format_duration, paint, render_histogram};
use crate::output::{
    BenchCase, CaseOutcome, ColorMode, DeviceInfo, LinkInfo, PeerAccess, PeerRoute,
};
use crate::stats::{DistributionShape, Summary};
use std::fmt::Write as _;

pub const P2P_DIAGNOSTIC_CSV_HEADER: &str = "schema_version,transport_verdict,transport_evidence,src_index,src_bdf,dst_index,dst_bdf,payload_bytes,samples,warmup_us,queue_group,benchmark_mode,timing_mode,stream_count,queue_streams,counter_scope,peer_access,route_class,route_detail,direct_status,direct_median_gb_s,direct_ci_lower_gb_s,direct_ci_upper_gb_s,direct_mad_gb_s,direct_distribution_shape,staged_status,staged_median_gb_s,staged_ci_lower_gb_s,staged_ci_upper_gb_s,staged_mad_gb_s,staged_distribution_shape,source_link,source_link_theoretical_gb_s,destination_link,destination_link_theoretical_gb_s,cpu_identity_status,cpu_identity,cpu_profile_status,cpu_profile,counter_explicit_staged_memory_status,counter_direct_memory_status,counter_direct_peer_write_status,counter_direct_peer_read_status,counter_direct_upi_status,acs_rollup,enabled_redirect_bits,acs_bridge_evidence,perfmon_upstream_commit,perfmon_license";

const SCHEMA_VERSION: &str = "3";
const COUNTER_SCOPE: &str = "system-wide uncore PMUs across repeated event-constrained runs; not transaction-tagged pair attribution";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiagnosticTextOptions {
    pub details: bool,
    pub color: ColorMode,
}

impl Default for DiagnosticTextOptions {
    fn default() -> Self {
        Self {
            details: false,
            color: ColorMode::Never,
        }
    }
}

pub fn render_p2p_diagnostic_text(report: &P2pDiagnosticReport) -> String {
    render_p2p_diagnostic_text_with_options(report, DiagnosticTextOptions::default())
}

pub fn render_p2p_diagnostic_text_with_options(
    report: &P2pDiagnosticReport,
    options: DiagnosticTextOptions,
) -> String {
    if options.details {
        return render_p2p_diagnostic_details_text(report, options);
    }

    render_p2p_diagnostic_concise_text(report, options)
}

fn render_p2p_diagnostic_concise_text(
    report: &P2pDiagnosticReport,
    options: DiagnosticTextOptions,
) -> String {
    let mut lines = render_concise_header(report, options);

    lines.push(String::new());
    lines.push(paint(options.color, "\u{1b}[1;36m", "Transfer"));
    append_case_measurement_compact(&mut lines, "direct", report.direct());
    append_case_measurement_compact(&mut lines, "explicit staged", report.staged());
    if let Some(ratio) = direct_to_staged_ratio(report.direct(), report.staged()) {
        lines.push(format!(
            "  comparison       {}x bandwidth; latency/CPU/engine overlap not measured",
            format_float_human(ratio, 2)
        ));
    } else if has_separated_clusters(report.direct()) || has_separated_clusters(report.staged()) {
        lines.push(
            "  comparison       omitted because separated clusters make a median ratio misleading"
                .to_owned(),
        );
    }
    append_compact_distribution(&mut lines, "direct", report.direct(), options.color);
    append_compact_distribution(
        &mut lines,
        "explicit staged",
        report.staged(),
        options.color,
    );
    append_concise_path(&mut lines, report, options.color);

    lines.push(String::new());
    lines.push(paint(options.color, "\u{1b}[1;36m", "Evidence"));
    lines.extend(render_evidence_checklist(report));

    if contains_permission_denied(report) {
        lines.push(String::new());
        lines.push(format!(
            "Permission needed: rerun the same settings with sudo for counter/ACS access: `{}`.",
            sudo_rerun_command(report)
        ));
    }

    lines.push(String::new());
    lines.push(paint(options.color, "\u{1b}[1;36m", "Limit"));
    lines.extend(
        confidence_summary(report.verdict())
            .iter()
            .map(|line| format!("  {line}")),
    );
    lines.push(paint(
        options.color,
        "\u{1b}[2m",
        "  More: --details shows raw counter gates and per-bridge ACSCtl values.",
    ));

    finish_lines(&lines)
}

fn render_concise_header(
    report: &P2pDiagnosticReport,
    options: DiagnosticTextOptions,
) -> Vec<String> {
    vec![
        paint(
            options.color,
            "\u{1b}[1;36m",
            &format!(
                "diag-p2p  dev{} ({}) -> dev{} ({})",
                report.source().index,
                device_bdf(report.source()),
                report.destination().index,
                device_bdf(report.destination())
            ),
        ),
        paint(
            options.color,
            verdict_color(report.verdict()),
            &format!("RESULT  {}", concise_verdict(report.verdict())),
        ),
        format!(
            "        Host-memory traffic {}",
            host_memory_summary(report.verdict())
        ),
        "        Physical route      not proven".to_owned(),
        paint(
            options.color,
            "\u{1b}[2m",
            &format!(
                "        Settings            {} | {} samples | {} warm-up",
                format_bytes(report.direct().byte_count),
                report.direct().requested_samples,
                format_duration(report.direct().warmup)
            ),
        ),
        paint(
            options.color,
            "\u{1b}[2m",
            &format!(
                "                            {} | {} | {}",
                report.direct().mode,
                report.direct().timing,
                queue_selection(report.direct())
            ),
        ),
    ]
}

fn append_concise_path(lines: &mut Vec<String>, report: &P2pDiagnosticReport, color: ColorMode) {
    lines.push(String::new());
    lines.push(paint(color, "\u{1b}[1;36m", "Path"));
    lines.push(format!("  topology     {}", route_class(report.route())));
    lines.push(format!(
        "  attachment   {}",
        route_attachment(report.route())
    ));
    lines.push(format!(
        "  peer access  {} (API capability only)",
        render_peer_access(report.peer_access())
    ));
    match measured_summary(report.direct()) {
        Some(summary) => {
            lines.push(format!(
                "  source link  {}",
                link_utilization_summary(summary, &report.source().pcie_link)
            ));
            lines.push(format!(
                "  dest link    {}",
                link_utilization_summary(summary, &report.destination().pcie_link)
            ));
        }
        None => lines.push("  links        unavailable; direct benchmark was skipped".to_owned()),
    }
    lines.push(format!("  ACS policy   {}", concise_acs_summary(report)));
}

fn render_p2p_diagnostic_details_text(
    report: &P2pDiagnosticReport,
    options: DiagnosticTextOptions,
) -> String {
    let source = report.source();
    let destination = report.destination();
    let mut lines = vec![
        format!(
            "diag-p2p details dev{} ({}) -> dev{} ({})",
            source.index,
            device_bdf(source),
            destination.index,
            device_bdf(destination)
        ),
        format!("Verdict: {}", concise_verdict(report.verdict())),
        format!(
            "Host-memory traffic: {}",
            host_memory_summary(report.verdict())
        ),
        format!("Settings: {}", render_effective_settings(report.direct())),
        format!(
            "Peer access API: {}",
            render_peer_access(report.peer_access())
        ),
        format!(
            "Route: {} ({})",
            route_class(report.route()),
            route_detail(report.route())
        ),
        format!("Evidence reasons: {}", render_reasons(report.reasons())),
    ];

    lines.push(String::new());
    lines.push("Transfer measurements".to_owned());
    lines.push(format!(
        "  direct          {}",
        render_case_measurement(report.direct())
    ));
    lines.push(format!(
        "  explicit staged {}",
        render_case_measurement(report.staged())
    ));
    if let Some(ratio) = direct_to_staged_ratio(report.direct(), report.staged()) {
        lines.push(format!(
            "  direct/staged ratio {}x bandwidth; latency, CPU overhead, and engine overlap not measured; not route proof",
            format_float_human(ratio, 2)
        ));
    } else if has_separated_clusters(report.direct()) || has_separated_clusters(report.staged()) {
        lines.push(
            "  direct/staged ratio omitted because separated clusters make a median ratio misleading"
                .to_owned(),
        );
    }
    append_compact_distribution(&mut lines, "direct", report.direct(), options.color);
    append_compact_distribution(
        &mut lines,
        "explicit staged",
        report.staged(),
        options.color,
    );
    lines.push(format!(
        "  source link      {}",
        render_theoretical(&source.pcie_link)
    ));
    lines.push(format!(
        "  destination link {}",
        render_theoretical(&destination.pcie_link)
    ));

    lines.push(String::new());
    lines.push("CPU and counters".to_owned());
    lines.push(
        "  scope system-wide uncore PMUs across repeated event-constrained runs; correlate on an idle host, not transaction-tagged pair attribution"
            .to_owned(),
    );
    lines.extend(render_cpu_lines(report.cpu()));
    for phase in report.phases() {
        lines.extend(render_phase_lines(phase));
    }

    lines.push(String::new());
    lines.push("ACS bridge evidence".to_owned());
    lines.push(format!("  rollup {}", acs_rollup(report.acs())));
    lines.extend(render_acs_lines(report.acs()));

    if contains_permission_denied(report) {
        lines.push(String::new());
        lines.push(format!(
            "Permission-limited evidence: rerun the same diagnostic settings with sudo for counter/ACS access: `{}`.",
            sudo_rerun_command(report)
        ));
    }

    finish_lines(&lines)
}

pub fn render_p2p_diagnostic_csv(report: &P2pDiagnosticReport) -> String {
    format!(
        "{P2P_DIAGNOSTIC_CSV_HEADER}\n{}",
        render_p2p_diagnostic_csv_row(report)
    )
}

fn render_p2p_diagnostic_csv_row(report: &P2pDiagnosticReport) -> String {
    let direct = case_summary(report.direct());
    let staged = case_summary(report.staged());
    let source = report.source();
    let destination = report.destination();
    let attribution = report.perfmon_attribution();
    let fields = vec![
        SCHEMA_VERSION.to_owned(),
        verdict_label(report.verdict()).to_owned(),
        transport_evidence(report),
        source.index.to_string(),
        source.pci_address.clone().unwrap_or_default(),
        destination.index.to_string(),
        destination.pci_address.clone().unwrap_or_default(),
        report.direct().byte_count.to_string(),
        report.direct().requested_samples.to_string(),
        report.direct().warmup.as_micros().to_string(),
        report
            .direct()
            .selected_group
            .as_ref()
            .map_or_else(String::new, |group| group.ordinal.to_string()),
        report.direct().mode.to_string(),
        report.direct().timing.to_string(),
        report.direct().streams.len().to_string(),
        render_queue_streams(report.direct()),
        COUNTER_SCOPE.to_owned(),
        render_peer_access(report.peer_access()).to_owned(),
        route_class(report.route()).to_owned(),
        route_detail(report.route()),
        direct.status,
        direct.median,
        direct.ci_lower,
        direct.ci_upper,
        direct.mad,
        direct.distribution_shape,
        staged.status,
        staged.median,
        staged.ci_lower,
        staged.ci_upper,
        staged.mad,
        staged.distribution_shape,
        render_link_csv(&source.pcie_link),
        render_link_theoretical_csv(&source.pcie_link),
        render_link_csv(&destination.pcie_link),
        render_link_theoretical_csv(&destination.pcie_link),
        availability_status(report.cpu().identity()).to_owned(),
        cpu_identity_value(report.cpu().identity()),
        availability_status(report.cpu().profile()).to_owned(),
        cpu_profile_value(report.cpu().profile()),
        phase_status(report, CounterPhase::ExplicitStagedMemory),
        phase_status(report, CounterPhase::DirectMemory),
        phase_status(report, CounterPhase::DirectPeerWrite),
        phase_status(report, CounterPhase::DirectPeerRead),
        phase_status(report, CounterPhase::DirectUpi),
        acs_rollup(report.acs()),
        enabled_redirect_bits(report.acs()),
        acs_bridge_evidence(report.acs()),
        attribution.upstream_commit.to_owned(),
        attribution.license.to_owned(),
    ];

    let mut output = fields
        .into_iter()
        .map(|field| csv_escape(&field))
        .collect::<Vec<_>>()
        .join(",");
    output.push('\n');
    output
}

struct RenderedCaseSummary {
    status: String,
    median: String,
    ci_lower: String,
    ci_upper: String,
    mad: String,
    distribution_shape: String,
}

fn concise_verdict(verdict: &DiagnosticVerdict) -> &'static str {
    match verdict {
        DiagnosticVerdict::CounterConsistentPeer {
            route_qualifier:
                Some(PeerRouteQualifier::AcsRedirectedOrUpstreamRoutedPeerTrafficNotHostBounce),
        } => "counter-consistent peer traffic (upstream-routed)",
        DiagnosticVerdict::CounterConsistentPeer {
            route_qualifier: None,
        } => "counter-consistent peer traffic",
        DiagnosticVerdict::CounterConsistentHostBounce => "counter-consistent host-memory traffic",
        DiagnosticVerdict::MixedSignalsAcrossRuns {
            route_qualifier:
                Some(PeerRouteQualifier::AcsRedirectedOrUpstreamRoutedPeerTrafficNotHostBounce),
        } => "mixed counter signals; ACS redirect policy enabled",
        DiagnosticVerdict::MixedSignalsAcrossRuns {
            route_qualifier: None,
        } => "mixed signals across repeated runs",
        DiagnosticVerdict::HeuristicOnly { likely } => match likely {
            LikelyMechanism::HostStaged => "heuristic only; likely host-staged",
            LikelyMechanism::DeviceSide => "heuristic only; likely device-side",
            LikelyMechanism::LinkLimited => "heuristic only; likely link-limited",
        },
        DiagnosticVerdict::Indeterminate => "indeterminate",
    }
}

fn verdict_color(verdict: &DiagnosticVerdict) -> &'static str {
    match verdict {
        DiagnosticVerdict::CounterConsistentPeer { .. } => "\u{1b}[1;32m",
        DiagnosticVerdict::CounterConsistentHostBounce
        | DiagnosticVerdict::MixedSignalsAcrossRuns { .. } => "\u{1b}[1;31m",
        DiagnosticVerdict::HeuristicOnly { .. } | DiagnosticVerdict::Indeterminate => {
            "\u{1b}[1;33m"
        }
    }
}

fn host_memory_summary(verdict: &DiagnosticVerdict) -> &'static str {
    match verdict {
        DiagnosticVerdict::CounterConsistentPeer { .. } => "below calibrated bounce gate",
        DiagnosticVerdict::CounterConsistentHostBounce => "consistent with explicit staging",
        DiagnosticVerdict::MixedSignalsAcrossRuns { .. } => "mixed/inconclusive",
        DiagnosticVerdict::HeuristicOnly { .. } | DiagnosticVerdict::Indeterminate => {
            "not counter-qualified"
        }
    }
}

fn render_effective_settings(case: &BenchCase) -> String {
    format!(
        "payload {}, {} samples, {} warm-up, {}, {}, {}",
        format_bytes(case.byte_count),
        case.requested_samples,
        format_duration(case.warmup),
        case.mode,
        case.timing,
        queue_selection(case)
    )
}

fn queue_selection(case: &BenchCase) -> String {
    case.selected_group.as_ref().map_or_else(
        || "all copy-capable queue groups".to_owned(),
        |group| format!("queue group {}", group.ordinal),
    )
}

fn append_case_measurement_compact(lines: &mut Vec<String>, label: &str, case: &BenchCase) {
    match &case.outcome {
        CaseOutcome::Measured {
            summary,
            samples_gb_s,
            ..
        } if matches!(summary.shape, DistributionShape::SeparatedClusters(_)) => {
            lines.push(format!(
                "  {label:<17}separated-cluster candidate; overall median omitted"
            ));
            if let Some((lower, upper)) = cluster_summaries(summary, samples_gb_s) {
                lines.push(format!(
                    "  {:17}lower {} GB/s (p10..p90 {}..{}; {}/{})",
                    "",
                    format_float_human(lower.center, 2),
                    format_float_human(lower.p10, 2),
                    format_float_human(lower.p90, 2),
                    lower.count,
                    summary.count
                ));
                lines.push(format!(
                    "  {:17}upper {} GB/s (p10..p90 {}..{}; {}/{})",
                    "",
                    format_float_human(upper.center, 2),
                    format_float_human(upper.p10, 2),
                    format_float_human(upper.p90, 2),
                    upper.count,
                    summary.count
                ));
            }
        }
        CaseOutcome::Measured { summary, .. } => lines.push(format!(
            "  {label:<17}{} GB/s median (p5..p95 {}..{}, MAD {})",
            format_float_human(summary.median, 2),
            format_float_human(summary.p5, 2),
            format_float_human(summary.p95, 2),
            format_float_human(summary.mad, 2)
        )),
        CaseOutcome::Skipped { reason } => {
            lines.push(format!("  {label:<17}skipped ({reason})"));
        }
    }
}

#[derive(Clone, Copy)]
struct ClusterSummary {
    center: f64,
    p10: f64,
    p90: f64,
    count: usize,
}

fn cluster_summaries(
    summary: &Summary,
    samples: &[f64],
) -> Option<(ClusterSummary, ClusterSummary)> {
    let DistributionShape::SeparatedClusters(clusters) = summary.shape else {
        return None;
    };
    if clusters.lower_count + clusters.upper_count != samples.len() {
        return None;
    }

    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let (lower, upper) = sorted.split_at(clusters.lower_count);
    Some((
        ClusterSummary {
            center: clusters.lower_center,
            p10: crate::stats::percentile(lower, 10.0)?,
            p90: crate::stats::percentile(lower, 90.0)?,
            count: clusters.lower_count,
        },
        ClusterSummary {
            center: clusters.upper_center,
            p10: crate::stats::percentile(upper, 10.0)?,
            p90: crate::stats::percentile(upper, 90.0)?,
            count: clusters.upper_count,
        },
    ))
}

fn append_compact_distribution(
    lines: &mut Vec<String>,
    label: &str,
    case: &BenchCase,
    color: ColorMode,
) {
    let CaseOutcome::Measured {
        summary,
        samples_gb_s,
        ..
    } = &case.outcome
    else {
        return;
    };
    let Some(histogram) = Histogram::from_samples(samples_gb_s, 6) else {
        return;
    };
    let median = matches!(summary.shape, DistributionShape::Ordinary).then_some(summary.median);
    let Some(rows) = render_histogram(&histogram, median, color) else {
        return;
    };

    lines.push(String::new());
    lines.push(format!(
        "{label} distribution GB/s ({} samples; bars normalized within this chart)",
        histogram.sample_count
    ));
    lines.extend(rows.into_iter().map(|row| format!("  {row}")));
}

fn link_utilization(rate_gb_s: f64, link: &LinkInfo) -> String {
    match link {
        LinkInfo::Known {
            generation,
            width,
            theoretical_gb_s,
        } if *theoretical_gb_s > 0.0 => format!(
            "Gen{generation} x{width}: {}% of theoretical",
            format_float_human(rate_gb_s / theoretical_gb_s * 100.0, 1)
        ),
        LinkInfo::Known {
            generation, width, ..
        } => format!("Gen{generation} x{width}: utilization unavailable"),
        LinkInfo::Unknown { reason } => format!("unknown link ({reason})"),
    }
}

fn link_utilization_summary(summary: &Summary, link: &LinkInfo) -> String {
    match summary.shape {
        DistributionShape::Ordinary => link_utilization(summary.median, link),
        DistributionShape::SeparatedClusters(clusters) => match link {
            LinkInfo::Known {
                generation,
                width,
                theoretical_gb_s,
            } if *theoretical_gb_s > 0.0 => format!(
                "Gen{generation} x{width}: lower {}%, upper {}% of theoretical",
                format_float_human(clusters.lower_center / theoretical_gb_s * 100.0, 1),
                format_float_human(clusters.upper_center / theoretical_gb_s * 100.0, 1)
            ),
            LinkInfo::Known {
                generation, width, ..
            } => format!("Gen{generation} x{width}: utilization unavailable"),
            LinkInfo::Unknown { reason } => format!("unknown link ({reason})"),
        },
    }
}

fn render_evidence_checklist(report: &P2pDiagnosticReport) -> Vec<String> {
    vec![
        format!(
            "  {} counter platform     {}/{}",
            checklist_mark(
                report.cpu().identity().is_available() && report.cpu().profile().is_available()
            ),
            checklist_status(report.cpu().identity()),
            checklist_status(report.cpu().profile())
        ),
        format!(
            "  {} staged calibration   {}",
            checklist_phase_mark(report, CounterPhase::ExplicitStagedMemory),
            checklist_phase(report, CounterPhase::ExplicitStagedMemory)
        ),
        format!(
            "  {} host-memory traffic  {}",
            verdict_evidence_mark(report.verdict()),
            host_memory_signal_summary(report.verdict())
        ),
        format!(
            "  {} peer-counter traffic {}",
            verdict_evidence_mark(report.verdict()),
            peer_signal_summary(report.verdict())
        ),
        format!(
            "  {} optional UPI         {} (supporting evidence only)",
            checklist_phase_mark(report, CounterPhase::DirectUpi),
            checklist_phase(report, CounterPhase::DirectUpi)
        ),
    ]
}

fn checklist_mark(qualified: bool) -> &'static str {
    if qualified { "✓" } else { "!" }
}

fn checklist_phase_mark(report: &P2pDiagnosticReport, phase: CounterPhase) -> &'static str {
    checklist_mark(
        report
            .phases()
            .iter()
            .find(|candidate| candidate.phase() == phase)
            .is_some_and(|candidate| {
                matches!(
                    candidate.evidence(),
                    EvidenceAvailability::Available(run) if run.terminal_failure().is_none()
                )
            }),
    )
}

fn verdict_evidence_mark(verdict: &DiagnosticVerdict) -> &'static str {
    match verdict {
        DiagnosticVerdict::CounterConsistentPeer { .. }
        | DiagnosticVerdict::CounterConsistentHostBounce
        | DiagnosticVerdict::MixedSignalsAcrossRuns { .. } => "✓",
        DiagnosticVerdict::HeuristicOnly { .. } | DiagnosticVerdict::Indeterminate => "!",
    }
}

fn host_memory_signal_summary(verdict: &DiagnosticVerdict) -> &'static str {
    match verdict {
        DiagnosticVerdict::CounterConsistentPeer { .. } => "below calibrated bounce gate",
        DiagnosticVerdict::CounterConsistentHostBounce => "above calibrated bounce gate",
        DiagnosticVerdict::MixedSignalsAcrossRuns { .. } => {
            "above calibrated gate in some repeated runs"
        }
        DiagnosticVerdict::HeuristicOnly { .. } | DiagnosticVerdict::Indeterminate => {
            "not counter-qualified"
        }
    }
}

fn peer_signal_summary(verdict: &DiagnosticVerdict) -> &'static str {
    match verdict {
        DiagnosticVerdict::CounterConsistentPeer { .. } => "observed",
        DiagnosticVerdict::MixedSignalsAcrossRuns { .. } => {
            "above calibrated gate in some repeated runs"
        }
        DiagnosticVerdict::CounterConsistentHostBounce
        | DiagnosticVerdict::HeuristicOnly { .. }
        | DiagnosticVerdict::Indeterminate => "not counter-qualified",
    }
}

fn confidence_summary(verdict: &DiagnosticVerdict) -> &'static [&'static str] {
    match verdict {
        DiagnosticVerdict::CounterConsistentPeer { .. }
        | DiagnosticVerdict::CounterConsistentHostBounce
        | DiagnosticVerdict::MixedSignalsAcrossRuns { .. } => &[
            "counter evidence uses system-wide PMUs; run on an idle host.",
            "It is not physical-route proof.",
        ],
        DiagnosticVerdict::HeuristicOnly { .. } => &[
            "heuristic only; hardware-counter evidence did not qualify.",
            "It is not physical-route proof.",
        ],
        DiagnosticVerdict::Indeterminate => {
            &["indeterminate; available evidence does not identify the transport mechanism."]
        }
    }
}

fn checklist_phase(report: &P2pDiagnosticReport, phase: CounterPhase) -> String {
    report
        .phases()
        .iter()
        .find(|candidate| candidate.phase() == phase)
        .map_or_else(
            || "missing".to_owned(),
            |phase| match phase.evidence() {
                EvidenceAvailability::Available(run) => run.terminal_failure().map_or_else(
                    || "ok".to_owned(),
                    |failure| {
                        format!(
                            "failed after {} samples ({})",
                            run.sample_windows().len(),
                            failure.message()
                        )
                    },
                ),
                availability => checklist_status(availability).to_owned(),
            },
        )
}

fn checklist_status<T>(availability: &EvidenceAvailability<T>) -> &'static str {
    match availability {
        EvidenceAvailability::Available(_) => "ok",
        EvidenceAvailability::PermissionDenied(_) => "permission needed",
        EvidenceAvailability::Unsupported(_) => "unsupported",
        EvidenceAvailability::Malformed(_) => "malformed",
        EvidenceAvailability::ResourceUnavailable(_) | EvidenceAvailability::Other(_) => {
            "unavailable"
        }
        EvidenceAvailability::Io(_) => "I/O failure",
    }
}

fn render_case_measurement(case: &BenchCase) -> String {
    match &case.outcome {
        CaseOutcome::Measured { summary, .. }
            if matches!(summary.shape, DistributionShape::SeparatedClusters(_)) =>
        {
            render_separated_clusters(summary, false)
        }
        CaseOutcome::Measured { summary, .. } => format!(
            "measured median {} GB/s, 95% CI {}..{} GB/s, MAD {} GB/s",
            format_float_human(summary.median, 2),
            format_float_human(summary.median_confidence.lower_bound, 2),
            format_float_human(summary.median_confidence.upper_bound, 2),
            format_float_human(summary.mad, 2)
        ),
        CaseOutcome::Skipped { reason } => format!("skipped: {reason}"),
    }
}

fn render_separated_clusters(summary: &Summary, compact: bool) -> String {
    let DistributionShape::SeparatedClusters(clusters) = summary.shape else {
        unreachable!("called only for separated clusters");
    };
    let prefix = if compact {
        "separated-cluster candidate"
    } else {
        "separated-cluster candidate measured"
    };
    format!(
        "{prefix}: ~{} GB/s ({}/{} samples) and ~{} GB/s ({}/{} samples); median {} GB/s is not representative",
        format_float_human(clusters.lower_center, 2),
        clusters.lower_count,
        summary.count,
        format_float_human(clusters.upper_center, 2),
        clusters.upper_count,
        summary.count,
        format_float_human(summary.median, 2)
    )
}

fn case_summary(case: &BenchCase) -> RenderedCaseSummary {
    match &case.outcome {
        CaseOutcome::Measured { summary, .. } => RenderedCaseSummary {
            status: "measured".to_owned(),
            median: format_float_csv(summary.median),
            ci_lower: format_float_csv(summary.median_confidence.lower_bound),
            ci_upper: format_float_csv(summary.median_confidence.upper_bound),
            mad: format_float_csv(summary.mad),
            distribution_shape: render_distribution_shape_csv(summary.shape),
        },
        CaseOutcome::Skipped { .. } => RenderedCaseSummary {
            status: "skipped".to_owned(),
            median: String::new(),
            ci_lower: String::new(),
            ci_upper: String::new(),
            mad: String::new(),
            distribution_shape: String::new(),
        },
    }
}

fn render_distribution_shape_csv(shape: DistributionShape) -> String {
    match shape {
        DistributionShape::Ordinary => "no-separated-cluster-detected".to_owned(),
        DistributionShape::SeparatedClusters(clusters) => format!(
            "separated-cluster-candidate:lower={:.6}/{};upper={:.6}/{}",
            clusters.lower_center,
            clusters.lower_count,
            clusters.upper_center,
            clusters.upper_count
        ),
    }
}

fn direct_to_staged_ratio(direct: &BenchCase, staged: &BenchCase) -> Option<f64> {
    let direct = measured_summary(direct)?;
    let staged = measured_summary(staged)?;
    if !matches!(direct.shape, DistributionShape::Ordinary)
        || !matches!(staged.shape, DistributionShape::Ordinary)
    {
        return None;
    }
    (staged.median > 0.0).then_some(direct.median / staged.median)
}

fn has_separated_clusters(case: &BenchCase) -> bool {
    measured_summary(case)
        .is_some_and(|summary| matches!(summary.shape, DistributionShape::SeparatedClusters(_)))
}

fn measured_summary(case: &BenchCase) -> Option<&Summary> {
    match &case.outcome {
        CaseOutcome::Measured { summary, .. } => Some(summary),
        CaseOutcome::Skipped { .. } => None,
    }
}

fn render_cpu_lines(cpu: &CpuEvidence) -> Vec<String> {
    vec![
        format!(
            "  identity {}{}",
            availability_status(cpu.identity()),
            availability_suffix(cpu.identity(), cpu_identity_value)
        ),
        format!(
            "  profile  {}{}",
            availability_status(cpu.profile()),
            availability_suffix(cpu.profile(), cpu_profile_value)
        ),
    ]
}

fn render_phase_lines(phase: &CounterPhaseReport) -> Vec<String> {
    let supporting = if phase.phase() == CounterPhase::DirectUpi {
        " (UPI supporting fabric evidence only)"
    } else {
        ""
    };
    let mut lines = vec![format!(
        "  {}: {}{}",
        phase_label(phase.phase()),
        evidence_availability_summary(phase.evidence()),
        supporting
    )];
    if !phase.missing_roles().is_empty() {
        lines.push(format!(
            "    missing roles {}",
            phase
                .missing_roles()
                .iter()
                .map(|role| format!("{} ({})", role.role(), role.reason()))
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }
    if let EvidenceAvailability::Available(run) = phase.evidence() {
        lines.push(format!(
            "    baselines {}, samples {}, idle baseline {}",
            run.baseline_windows().len(),
            run.sample_windows().len(),
            format_duration(run.idle_baseline_duration())
        ));
        if let Some(failure) = run.terminal_failure() {
            lines.push(format!("    terminal failure {}", render_failure(failure)));
        }
        lines.push(format!("    role counts {}", role_count_summary(run)));
    } else if let Some(failure) = phase.evidence().failure() {
        lines.push(format!("    reason {}", render_failure(failure)));
    }
    lines
}

fn role_count_summary(run: &CounterRunReport) -> String {
    let mut roles = Vec::<EventRole>::new();
    for sample in run.sample_windows() {
        for total in sample.window().role_totals() {
            if !roles.contains(&total.role()) {
                roles.push(total.role());
            }
        }
    }
    roles.sort_by_key(std::string::ToString::to_string);
    if roles.is_empty() {
        return "none".to_owned();
    }

    roles
        .into_iter()
        .map(|role| {
            let mut values = run
                .sample_windows()
                .iter()
                .filter_map(|sample| {
                    sample
                        .window()
                        .role_totals()
                        .iter()
                        .find(|total| total.role() == role)
                        .map(|total| total.value())
                })
                .collect::<Vec<_>>();
            values.sort_unstable();
            let p10 = percentile_nearest(&values, 10).unwrap_or(0);
            let median = percentile_nearest(&values, 50).unwrap_or(0);
            let range = match (values.first(), values.last()) {
                (Some(first), Some(last)) if first != last => format!(", range {first}..{last}"),
                _ => String::new(),
            };
            format!("{role}: p10 {p10}, median {median}{range}")
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn percentile_nearest(sorted_values: &[u64], percentile: usize) -> Option<u64> {
    if sorted_values.is_empty() {
        return None;
    }
    let rank = ((sorted_values.len() - 1) * percentile + 50) / 100;
    sorted_values.get(rank).copied()
}

fn render_acs_lines(
    acs: &EvidenceAvailability<crate::evidence::AcsBridgePathEvidence>,
) -> Vec<String> {
    if let EvidenceAvailability::Available(path) = acs {
        if path.bridges().is_empty() {
            return vec!["  no bridge evidence".to_owned()];
        }

        return path
            .bridges()
            .iter()
            .map(|bridge| {
                let result = match bridge.result() {
                    Ok(AcsBridgeOutcome::RedirectObserved(capability)) => format!(
                        "ACS @0x{:03x}: ACSCap=0x{:04x}, ACSCtl=0x{:04x}; RR/CR policy enabled with {} bits; enabled ACS controls {}",
                        capability.offset(),
                        capability.raw_capability(),
                        capability.raw_control(),
                        redirect_bits(capability.enabled()),
                        acs_control_bits(capability.enabled())
                    ),
                    Ok(AcsBridgeOutcome::NoRedirectObserved(capability)) => format!(
                        "ACS @0x{:03x}: ACSCap=0x{:04x}, ACSCtl=0x{:04x}; RR/CR policy disabled (not proof of direct); enabled ACS controls {}",
                        capability.offset(),
                        capability.raw_capability(),
                        capability.raw_control(),
                        acs_control_bits(capability.enabled())
                    ),
                    Ok(AcsBridgeOutcome::ExtendedConfigUnavailable { config_bytes }) => {
                        format!(
                            "extended config unavailable ({config_bytes} config bytes; full extended config may require elevated privileges)"
                        )
                    }
                    Ok(AcsBridgeOutcome::NoCapability) => "no ACS capability".to_owned(),
                    Err(AcsReadFailure::PermissionDenied { path, error }) => {
                        format!("permission failure reading {}: {error}", path.display())
                    }
                    Err(AcsReadFailure::NotFound { path }) => {
                        format!("I/O failure: missing {}", path.display())
                    }
                    Err(AcsReadFailure::Io { path, error }) => {
                        format!("I/O failure reading {}: {error}", path.display())
                    }
                    Err(AcsReadFailure::Malformed { path, reason }) => format!(
                        "malformed PCI config in {}: {}",
                        path.display(),
                        render_malformed_acs(reason)
                    ),
                };
                format!(
                    "  bridge {} ({}): {result}",
                    bridge.bridge(),
                    bridge.sysfs_path().join("config").display()
                )
            })
            .collect();
    }

    let failure = acs
        .failure()
        .map_or_else(|| "unavailable".to_owned(), render_failure);
    vec![format!("  unavailable: {failure}")]
}

fn acs_rollup(acs: &EvidenceAvailability<crate::evidence::AcsBridgePathEvidence>) -> String {
    let EvidenceAvailability::Available(path) = acs else {
        return acs.failure().map_or_else(
            || "unavailable".to_owned(),
            |failure| format!("unavailable:{}", failure_kind_label(failure.kind())),
        );
    };
    let mut saw_no_redirect = false;
    let mut saw_extended_unavailable = false;
    let mut saw_no_capability = false;
    let mut saw_failure = false;
    let mut redirect_bridges = Vec::new();
    for bridge in path.bridges() {
        match bridge.result() {
            Ok(AcsBridgeOutcome::RedirectObserved(_)) => {
                redirect_bridges.push(bridge.bridge().to_string());
            }
            Ok(AcsBridgeOutcome::NoRedirectObserved(_)) => saw_no_redirect = true,
            Ok(AcsBridgeOutcome::ExtendedConfigUnavailable { .. }) => {
                saw_extended_unavailable = true;
            }
            Ok(AcsBridgeOutcome::NoCapability) => saw_no_capability = true,
            Err(_) => saw_failure = true,
        }
    }
    if !redirect_bridges.is_empty() {
        format!("RR/CR policy enabled on {}", redirect_bridges.join(";"))
    } else if saw_failure {
        "bridge read failure".to_owned()
    } else if saw_extended_unavailable {
        "extended config unavailable".to_owned()
    } else if saw_no_redirect {
        "RR/CR policy disabled on readable ACS bridges (not proof of direct)".to_owned()
    } else if saw_no_capability {
        "no ACS capability".to_owned()
    } else {
        "no bridge evidence".to_owned()
    }
}

fn concise_acs_summary(report: &P2pDiagnosticReport) -> String {
    let EvidenceAvailability::Available(path) = report.acs() else {
        return acs_rollup(report.acs());
    };
    let mut readable = 0;
    let mut redirect = 0;
    for bridge in path.bridges() {
        match bridge.result() {
            Ok(AcsBridgeOutcome::RedirectObserved(_)) => {
                readable += 1;
                redirect += 1;
            }
            Ok(AcsBridgeOutcome::NoRedirectObserved(_)) => readable += 1,
            Ok(
                AcsBridgeOutcome::ExtendedConfigUnavailable { .. } | AcsBridgeOutcome::NoCapability,
            )
            | Err(_) => {}
        }
    }

    let mut summary = if redirect > 0 {
        format!("RR/CR enabled on {redirect}/{readable} readable ACS bridges")
    } else if readable > 0 {
        format!("RR/CR disabled on {readable} readable ACS bridges")
    } else {
        acs_rollup(report.acs())
    };
    if report
        .reasons()
        .iter()
        .any(|reason| reason.code() == crate::diagnostics::EvidenceReasonCode::AcsStateChanged)
    {
        summary.push_str("; changed during run, final snapshot shown");
    } else if readable > 0 {
        summary.push_str("; bridge IDs in --details");
    }
    summary
}

fn acs_bridge_evidence(
    acs: &EvidenceAvailability<crate::evidence::AcsBridgePathEvidence>,
) -> String {
    let EvidenceAvailability::Available(path) = acs else {
        return String::new();
    };
    path.bridges()
        .iter()
        .map(|bridge| {
            let outcome = acs_csv_outcome(bridge.result());
            format!(
                "{}@{}={outcome}",
                bridge.bridge(),
                bridge.sysfs_path().join("config").display()
            )
        })
        .collect::<Vec<_>>()
        .join(";")
}

fn acs_csv_outcome(result: &std::result::Result<AcsBridgeOutcome, AcsReadFailure>) -> String {
    match result {
        Ok(
            AcsBridgeOutcome::RedirectObserved(capability)
            | AcsBridgeOutcome::NoRedirectObserved(capability),
        ) => format!(
            "acs@0x{:03x}:cap=0x{:04x}/ctl=0x{:04x}",
            capability.offset(),
            capability.raw_capability(),
            capability.raw_control()
        ),
        Ok(AcsBridgeOutcome::ExtendedConfigUnavailable { config_bytes }) => {
            format!("extended-config-unavailable:{config_bytes}-bytes")
        }
        Ok(AcsBridgeOutcome::NoCapability) => "no-acs-capability".to_owned(),
        Err(AcsReadFailure::PermissionDenied { path, error }) => {
            format!("permission-denied:{}:{error}", path.display())
        }
        Err(AcsReadFailure::NotFound { path }) => {
            format!("not-found:{}", path.display())
        }
        Err(AcsReadFailure::Io { path, error }) => {
            format!("io-failure:{}:{error}", path.display())
        }
        Err(AcsReadFailure::Malformed { path, reason }) => {
            format!("malformed:{}:{reason}", path.display())
        }
    }
}

fn enabled_redirect_bits(
    acs: &EvidenceAvailability<crate::evidence::AcsBridgePathEvidence>,
) -> String {
    let EvidenceAvailability::Available(path) = acs else {
        return String::new();
    };
    let mut bits = Vec::new();
    for bridge in path.bridges() {
        if let Ok(AcsBridgeOutcome::RedirectObserved(capability)) = bridge.result() {
            for bit in redirect_bit_names(capability.enabled()) {
                if !bits.contains(&bit) {
                    bits.push(bit);
                }
            }
        }
    }
    bits.join(";")
}

fn redirect_bits(flags: AcsFlags) -> String {
    let names = redirect_bit_names(flags);
    if names.is_empty() {
        "no redirect".to_owned()
    } else {
        names.join("/")
    }
}

fn redirect_bit_names(flags: AcsFlags) -> Vec<&'static str> {
    let mut names = Vec::new();
    if flags.p2p_request_redirect() {
        names.push("RR");
    }
    if flags.p2p_completion_redirect() {
        names.push("CR");
    }
    names
}

fn acs_control_bits(flags: AcsFlags) -> String {
    let mut names = Vec::new();
    if flags.source_validation() {
        names.push("SV");
    }
    if flags.translation_blocking() {
        names.push("TB");
    }
    if flags.p2p_request_redirect() {
        names.push("RR");
    }
    if flags.p2p_completion_redirect() {
        names.push("CR");
    }
    if flags.upstream_forwarding() {
        names.push("UF");
    }
    if flags.p2p_egress_control() {
        names.push("EC");
    }
    if flags.direct_translated_p2p() {
        names.push("DT");
    }
    if names.is_empty() {
        "none".to_owned()
    } else {
        names.join("/")
    }
}

fn contains_permission_denied(report: &P2pDiagnosticReport) -> bool {
    availability_permission_denied(report.cpu().identity())
        || availability_permission_denied(report.cpu().profile())
        || report
            .phases()
            .iter()
            .any(|phase| availability_permission_denied(phase.evidence()))
        || availability_permission_denied(report.acs())
        || acs_bridge_permission_denied(report.acs())
}

fn acs_bridge_permission_denied(
    acs: &EvidenceAvailability<crate::evidence::AcsBridgePathEvidence>,
) -> bool {
    match acs {
        EvidenceAvailability::Available(path) => path.bridges().iter().any(|bridge| {
            matches!(
                bridge.result(),
                Err(AcsReadFailure::PermissionDenied { .. })
            )
        }),
        _ => false,
    }
}

fn availability_permission_denied<T>(availability: &EvidenceAvailability<T>) -> bool {
    matches!(availability, EvidenceAvailability::PermissionDenied(_))
}

fn phase_status(report: &P2pDiagnosticReport, phase: CounterPhase) -> String {
    report
        .phases()
        .iter()
        .find(|candidate| candidate.phase() == phase)
        .map_or_else(
            || "missing".to_owned(),
            |candidate| match candidate.evidence() {
                EvidenceAvailability::Available(run) if run.terminal_failure().is_some() => {
                    "terminal-failure".to_owned()
                }
                availability => availability_status(availability).to_owned(),
            },
        )
}

fn transport_evidence(report: &P2pDiagnosticReport) -> String {
    format!(
        "request=one Level Zero direct GPU-memory copy request; direct={}; staged={}; counter_scope={COUNTER_SCOPE}; peer_access={}; route_class={}; acs={}; reasons={}",
        case_evidence_status(report.direct()),
        case_evidence_status(report.staged()),
        render_peer_access(report.peer_access()),
        route_class(report.route()),
        acs_rollup(report.acs()),
        render_reasons(report.reasons())
    )
}

fn case_evidence_status(case: &BenchCase) -> String {
    match &case.outcome {
        CaseOutcome::Measured { .. } => "measured".to_owned(),
        CaseOutcome::Skipped { reason } => format!("skipped ({reason})"),
    }
}

fn render_queue_streams(case: &BenchCase) -> String {
    case.streams
        .iter()
        .map(|stream| format!("{}:{}", stream.group_ordinal, stream.queue_index))
        .collect::<Vec<_>>()
        .join(";")
}

fn render_reasons(reasons: &[EvidenceReason]) -> String {
    if reasons.is_empty() {
        return "none recorded".to_owned();
    }
    reasons
        .iter()
        .map(|reason| format!("{:?}: {}", reason.code(), reason.message()))
        .collect::<Vec<_>>()
        .join("; ")
}

fn evidence_availability_summary<T>(availability: &EvidenceAvailability<T>) -> String {
    match availability {
        EvidenceAvailability::Available(_) => "available".to_owned(),
        _ => availability.failure().map_or_else(
            || "unavailable".to_owned(),
            |failure| format!("unavailable ({})", render_failure(failure)),
        ),
    }
}

fn availability_status<T>(availability: &EvidenceAvailability<T>) -> &'static str {
    match availability {
        EvidenceAvailability::Available(_) => "available",
        EvidenceAvailability::PermissionDenied(_) => "permission-denied",
        EvidenceAvailability::Unsupported(_) => "unsupported",
        EvidenceAvailability::Malformed(_) => "malformed",
        EvidenceAvailability::ResourceUnavailable(_) => "resource-unavailable",
        EvidenceAvailability::Io(_) => "io-failure",
        EvidenceAvailability::Other(_) => "other-failure",
    }
}

fn availability_suffix<T>(
    availability: &EvidenceAvailability<T>,
    value: impl Fn(&EvidenceAvailability<T>) -> String,
) -> String {
    match availability {
        EvidenceAvailability::Available(_) => format!(" ({})", value(availability)),
        _ => availability
            .failure()
            .map(|failure| format!(" ({})", render_failure(failure)))
            .unwrap_or_default(),
    }
}

fn render_failure(failure: &EvidenceFailure) -> String {
    format!(
        "{}: {}",
        failure_kind_label(failure.kind()),
        failure.message()
    )
}

fn failure_kind_label(kind: EvidenceFailureKind) -> &'static str {
    match kind {
        EvidenceFailureKind::PermissionDenied => "permission-denied",
        EvidenceFailureKind::Unsupported => "unsupported",
        EvidenceFailureKind::Malformed => "malformed",
        EvidenceFailureKind::ResourceUnavailable => "resource-unavailable",
        EvidenceFailureKind::Io => "io-failure",
        EvidenceFailureKind::CounterOverflow => "counter-overflow",
        EvidenceFailureKind::TopologyMismatch => "topology-mismatch",
        EvidenceFailureKind::Other => "other-failure",
    }
}

fn cpu_identity_value(identity: &EvidenceAvailability<CpuIdentity>) -> String {
    match identity {
        EvidenceAvailability::Available(identity) => identity.to_string(),
        _ => String::new(),
    }
}

fn cpu_profile_value(profile: &EvidenceAvailability<CpuProfile>) -> String {
    match profile {
        EvidenceAvailability::Available(profile) => profile.name().to_owned(),
        _ => String::new(),
    }
}

fn verdict_label(verdict: &DiagnosticVerdict) -> &'static str {
    match verdict {
        DiagnosticVerdict::CounterConsistentPeer { .. } => "counter-consistent-peer",
        DiagnosticVerdict::CounterConsistentHostBounce => "counter-consistent-host-memory-traffic",
        DiagnosticVerdict::MixedSignalsAcrossRuns { .. } => "mixed-signals-across-repeated-runs",
        DiagnosticVerdict::HeuristicOnly { likely } => match likely {
            LikelyMechanism::HostStaged => "heuristic-only:host-staged",
            LikelyMechanism::DeviceSide => "heuristic-only:device-side",
            LikelyMechanism::LinkLimited => "heuristic-only:link-limited",
        },
        DiagnosticVerdict::Indeterminate => "indeterminate",
    }
}

fn render_peer_access(peer_access: &PeerAccess) -> &str {
    match peer_access {
        PeerAccess::Yes => "yes",
        PeerAccess::No => "no",
        PeerAccess::Unknown(_) => "unknown",
    }
}

fn route_class(route: &PeerRoute) -> &'static str {
    match route {
        PeerRoute::SameRootPort { .. } => "same-root-port",
        PeerRoute::SharedUpstreamBridge { .. } => "shared-upstream-bridge",
        PeerRoute::DifferentRootPorts { .. } => "different-root-ports",
        PeerRoute::CrossHostBridges { .. } => "cross-host-bridges",
        PeerRoute::Unknown(_) => "unknown",
    }
}

fn route_detail(route: &PeerRoute) -> String {
    match route {
        PeerRoute::SameRootPort { root_port } => format!("root_port={root_port}"),
        PeerRoute::SharedUpstreamBridge { common_bridge } => {
            format!("common_bridge={common_bridge}")
        }
        PeerRoute::DifferentRootPorts {
            host_bridge,
            source_root_port,
            destination_root_port,
        } => format!(
            "host_bridge={host_bridge};source_root_port={source_root_port};destination_root_port={destination_root_port}"
        ),
        PeerRoute::CrossHostBridges {
            source_host_bridge,
            destination_host_bridge,
        } => format!(
            "source_host_bridge={source_host_bridge};destination_host_bridge={destination_host_bridge}"
        ),
        PeerRoute::Unknown(reason) => reason.clone(),
    }
}

fn route_attachment(route: &PeerRoute) -> String {
    match route {
        PeerRoute::SameRootPort { root_port } => format!("root port {root_port}"),
        PeerRoute::SharedUpstreamBridge { common_bridge } => {
            format!("shared bridge {common_bridge}")
        }
        PeerRoute::DifferentRootPorts {
            host_bridge,
            source_root_port,
            destination_root_port,
        } => format!("{host_bridge}; root ports {source_root_port} -> {destination_root_port}"),
        PeerRoute::CrossHostBridges {
            source_host_bridge,
            destination_host_bridge,
        } => format!("host bridges {source_host_bridge} -> {destination_host_bridge}"),
        PeerRoute::Unknown(reason) => reason.clone(),
    }
}

fn render_theoretical(link: &LinkInfo) -> String {
    match link {
        LinkInfo::Known {
            theoretical_gb_s, ..
        } => format!(
            "{} negotiated theoretical GB/s",
            format_float_human(*theoretical_gb_s, 2)
        ),
        LinkInfo::Unknown { reason } => format!("unavailable ({reason})"),
    }
}

fn render_link_csv(link: &LinkInfo) -> String {
    match link {
        LinkInfo::Known {
            generation, width, ..
        } => format!("Gen{generation}x{width}"),
        LinkInfo::Unknown { reason } => format!("unknown:{reason}"),
    }
}

fn render_link_theoretical_csv(link: &LinkInfo) -> String {
    match link {
        LinkInfo::Known {
            theoretical_gb_s, ..
        } => format_float_csv(*theoretical_gb_s),
        LinkInfo::Unknown { .. } => String::new(),
    }
}

fn device_bdf(device: &DeviceInfo) -> &str {
    device.pci_address.as_deref().unwrap_or("unknown BDF")
}

fn phase_label(phase: CounterPhase) -> &'static str {
    match phase {
        CounterPhase::ExplicitStagedMemory => "explicit staged memory counters",
        CounterPhase::DirectMemory => "direct memory counters",
        CounterPhase::DirectPeerWrite => "direct peer-write counters",
        CounterPhase::DirectPeerRead => "direct peer-read counters",
        CounterPhase::DirectUpi => "direct UPI counters",
    }
}

fn render_malformed_acs(reason: &AcsMalformedConfig) -> String {
    reason.to_string()
}

fn format_float_csv(value: f64) -> String {
    if value.is_finite() {
        format!("{value:.6}")
    } else {
        String::new()
    }
}

fn format_float_human(value: f64, max_decimals: usize) -> String {
    if !value.is_finite() {
        return "unknown".to_owned();
    }
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

fn absolute_release_binary() -> String {
    if let Ok(current_exe) = std::env::current_exe()
        && current_exe.is_absolute()
    {
        return current_exe.display().to_string();
    }
    "/absolute/path/to/xfer".to_owned()
}

fn sudo_rerun_command(report: &P2pDiagnosticReport) -> String {
    let direct = report.direct();
    let mut command = format!(
        "sudo {} diag-p2p --device {} --peer-device {}",
        shell_quote(&absolute_release_binary()),
        report.source().index,
        report.destination().index
    );
    if let Some(group) = &direct.selected_group {
        write!(&mut command, " --queue-group {}", group.ordinal)
            .expect("writing to String cannot fail");
    }
    write!(
        &mut command,
        " --size {} --samples {} --warmup {}us",
        direct.byte_count,
        direct.requested_samples,
        direct.warmup.as_micros()
    )
    .expect("writing to String cannot fail");
    command
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"/._-".contains(&byte))
    {
        return value.to_owned();
    }
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn finish_lines(lines: &[String]) -> String {
    let mut output = lines.join("\n");
    output.push('\n');
    output
}

#[allow(clippy::too_many_lines)]
#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use crate::cli::{BenchMode, TimingMode, TransferClass};
    use crate::diagnostics::{
        CounterGroupReport, CounterPhaseReport, CounterRunReport, CounterSample, CounterWindow,
        CpuEvidence, DiagnosticVerdict, EvidenceAvailability, EvidenceFailure, EvidenceFailureKind,
        EvidenceReason, EvidenceReasonCode, LikelyMechanism, P2pDiagnosticReport, RoleCounterTotal,
    };
    use crate::evidence::intel_perfmon::{ATTRIBUTION, EventRole};
    use crate::evidence::{
        CounterSnapshot, CpuIdentity, CpuModel, CpuProfile, CpuVendor, EvidenceRunId,
        read_acs_bridge_path_from_sysfs,
    };
    use crate::output::{
        AllocationKind, BenchCase, CaseOutcome, DeviceInfo, Endpoint, LinkInfo, Operation,
        PeerAccess, PeerRoute, QueueFlags, QueueGroupInfo, QueueStreamInfo,
    };
    use crate::pcie::PciAddress;
    use crate::stats;

    use super::*;

    const SYSFS_PCI_DEVICES: &str = "bus/pci/devices";
    const PCI_EXT_CAP_START: usize = 0x100;
    const PCI_EXT_CAP_ID_ACS: u16 = 0x000d;
    const PCI_EXT_CAP_NEXT_MASK: u32 = 0x0fff;
    const PCI_EXT_CAP_NEXT_SHIFT: u32 = 20;
    const ACS_CAPABILITY_OFFSET: usize = 0x04;
    const ACS_CONTROL_OFFSET: usize = 0x06;
    const ACS_BODY_LEN: usize = ACS_CONTROL_OFFSET + 2;

    #[test]
    fn csv_header_is_stable() {
        assert_eq!(
            P2P_DIAGNOSTIC_CSV_HEADER,
            "schema_version,transport_verdict,transport_evidence,src_index,src_bdf,dst_index,dst_bdf,payload_bytes,samples,warmup_us,queue_group,benchmark_mode,timing_mode,stream_count,queue_streams,counter_scope,peer_access,route_class,route_detail,direct_status,direct_median_gb_s,direct_ci_lower_gb_s,direct_ci_upper_gb_s,direct_mad_gb_s,direct_distribution_shape,staged_status,staged_median_gb_s,staged_ci_lower_gb_s,staged_ci_upper_gb_s,staged_mad_gb_s,staged_distribution_shape,source_link,source_link_theoretical_gb_s,destination_link,destination_link_theoretical_gb_s,cpu_identity_status,cpu_identity,cpu_profile_status,cpu_profile,counter_explicit_staged_memory_status,counter_direct_memory_status,counter_direct_peer_write_status,counter_direct_peer_read_status,counter_direct_upi_status,acs_rollup,enabled_redirect_bits,acs_bridge_evidence,perfmon_upstream_commit,perfmon_license"
        );
        assert_eq!(P2P_DIAGNOSTIC_CSV_HEADER.split(',').count(), 49);

        let csv = render_p2p_diagnostic_csv(&base_report(DiagnosticVerdict::Indeterminate));
        assert_eq!(csv.lines().next(), Some(P2P_DIAGNOSTIC_CSV_HEADER));
        assert_eq!(csv.lines().count(), 2);
    }

    #[test]
    fn csv_row_escapes_and_matches_header_columns() {
        let mut report = base_report(DiagnosticVerdict::Indeterminate);
        let direct = report.direct().clone();
        let mut source = report.source().clone();
        source.pci_address = Some("0000:01:00.0,\"quoted\"".to_owned());
        report = P2pDiagnosticReport::new(
            source,
            report.destination().clone(),
            PeerAccess::Unknown("bad, maybe\nunknown".to_owned()),
            PeerRoute::Unknown("route, \"quoted\"".to_owned()),
            direct,
            report.staged().clone(),
            report.cpu().clone(),
            report.phases().to_vec(),
            report.acs().clone(),
            DiagnosticVerdict::Indeterminate,
            vec![EvidenceReason::new(
                EvidenceReasonCode::HeuristicUnavailable,
                "line one\nline two, quoted",
            )],
            ATTRIBUTION,
        );

        let row = render_p2p_diagnostic_csv_row(&report);
        let fields = split_csv_record(row.trim_end());

        assert_eq!(fields.len(), P2P_DIAGNOSTIC_CSV_HEADER.split(',').count());
        assert!(row.contains("\"0000:01:00.0,\"\"quoted\"\"\""));
        assert!(row.contains("\"route, \"\"quoted\"\"\""));
        assert!(row.contains("\"request=one Level Zero direct GPU-memory copy request"));
        assert!(!row.contains("\u{1b}["));
        assert_eq!(csv_field(&fields, "payload_bytes"), "2147483648");
        assert_eq!(csv_field(&fields, "samples"), "50");
        assert_eq!(csv_field(&fields, "warmup_us"), "1000000");
        assert_eq!(csv_field(&fields, "benchmark_mode"), "saturation");
        assert_eq!(csv_field(&fields, "timing_mode"), "wall-clock");
        assert_eq!(csv_field(&fields, "counter_scope"), COUNTER_SCOPE);
    }

    #[test]
    fn csv_acs_failure_outcomes_keep_typed_details() {
        let path = PathBuf::from("/sys/bus/pci/devices/0000:00:01.0/config");
        let permission = Err(AcsReadFailure::PermissionDenied {
            path: path.clone(),
            error: "denied".to_owned(),
        });
        let not_found = Err(AcsReadFailure::NotFound { path: path.clone() });
        let io = Err(AcsReadFailure::Io {
            path: path.clone(),
            error: "short read".to_owned(),
        });
        let malformed = Err(AcsReadFailure::Malformed {
            path,
            reason: AcsMalformedConfig::TruncatedCapabilityBody {
                offset: 0x100,
                config_bytes: 0x104,
            },
        });

        assert!(acs_csv_outcome(&permission).starts_with("permission-denied:"));
        assert!(acs_csv_outcome(&not_found).starts_with("not-found:"));
        assert!(acs_csv_outcome(&io).starts_with("io-failure:"));
        assert!(acs_csv_outcome(&malformed).starts_with("malformed:"));
    }

    #[test]
    fn renders_each_verdict_label() {
        for (verdict, text_expected, csv_expected) in [
            (
                DiagnosticVerdict::CounterConsistentPeer {
                    route_qualifier: None,
                },
                "RESULT  counter-consistent peer traffic",
                "counter-consistent-peer",
            ),
            (
                DiagnosticVerdict::CounterConsistentPeer {
                    route_qualifier: Some(
                        PeerRouteQualifier::AcsRedirectedOrUpstreamRoutedPeerTrafficNotHostBounce,
                    ),
                },
                "RESULT  counter-consistent peer traffic (upstream-routed)",
                "counter-consistent-peer",
            ),
            (
                DiagnosticVerdict::CounterConsistentHostBounce,
                "RESULT  counter-consistent host-memory traffic",
                "counter-consistent-host-memory-traffic",
            ),
            (
                DiagnosticVerdict::MixedSignalsAcrossRuns {
                    route_qualifier: None,
                },
                "RESULT  mixed signals across repeated runs",
                "mixed-signals-across-repeated-runs",
            ),
            (
                DiagnosticVerdict::HeuristicOnly {
                    likely: LikelyMechanism::HostStaged,
                },
                "RESULT  heuristic only; likely host-staged",
                "heuristic-only:host-staged",
            ),
            (
                DiagnosticVerdict::Indeterminate,
                "RESULT  indeterminate",
                "indeterminate",
            ),
        ] {
            let output = render_p2p_diagnostic_text(&base_report(verdict.clone()));
            assert!(output.contains(text_expected));
            assert!(render_p2p_diagnostic_csv(&base_report(verdict)).contains(csv_expected));
        }
    }

    #[test]
    fn concise_text_surfaces_acs_routed_peer_semantics() {
        let output =
            render_p2p_diagnostic_text(&base_report(DiagnosticVerdict::CounterConsistentPeer {
                route_qualifier: Some(
                    PeerRouteQualifier::AcsRedirectedOrUpstreamRoutedPeerTrafficNotHostBounce,
                ),
            }));

        assert!(output.contains("RESULT  counter-consistent peer traffic (upstream-routed)"));
        assert!(output.contains("Host-memory traffic below calibrated bounce gate"));
        assert!(output.contains("Physical route      not proven"));
        assert!(!output.contains("endpoint-direct physical routing observed"));
    }

    #[test]
    fn concise_confidence_matches_verdict_strength() {
        let heuristic =
            render_p2p_diagnostic_text(&base_report(DiagnosticVerdict::HeuristicOnly {
                likely: LikelyMechanism::DeviceSide,
            }));
        let indeterminate =
            render_p2p_diagnostic_text(&base_report(DiagnosticVerdict::Indeterminate));

        assert!(heuristic.contains("heuristic only; hardware-counter evidence did not qualify"));
        assert!(!heuristic.contains("counter evidence uses system-wide PMUs"));
        assert!(indeterminate.contains(
            "indeterminate; available evidence does not identify the transport mechanism"
        ));
    }

    #[test]
    fn concise_default_report_avoids_wide_terminal_lines() {
        let output = render_p2p_diagnostic_text(&base_report(
            DiagnosticVerdict::CounterConsistentHostBounce,
        ));
        let wide_lines = output
            .lines()
            .filter(|line| line.chars().count() > 88)
            .collect::<Vec<_>>();

        assert!(wide_lines.is_empty(), "wide lines: {wide_lines:#?}");
    }

    #[test]
    fn concise_default_report_separates_transfer_blocks() {
        let output = render_p2p_diagnostic_text(&base_report(
            DiagnosticVerdict::CounterConsistentHostBounce,
        ));
        let lines = output.lines().collect::<Vec<_>>();

        for heading in [
            "Transfer",
            "direct distribution GB/s",
            "explicit staged distribution GB/s",
        ] {
            let index = lines
                .iter()
                .position(|line| line.starts_with(heading))
                .unwrap_or_else(|| panic!("missing heading: {heading}"));
            assert_eq!(lines[index - 1], "", "missing blank line before {heading}");
        }
        assert!(
            lines.windows(2).all(|pair| pair != ["", ""]),
            "unexpected adjacent blank lines"
        );
    }

    #[test]
    fn reports_permission_denied_with_sudo_footer() {
        let mut report = base_report(DiagnosticVerdict::Indeterminate);
        report = P2pDiagnosticReport::new(
            report.source().clone(),
            report.destination().clone(),
            report.peer_access().clone(),
            report.route().clone(),
            report.direct().clone(),
            report.staged().clone(),
            report.cpu().clone(),
            vec![CounterPhaseReport::unavailable(
                CounterPhase::DirectMemory,
                1024,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                EvidenceFailure::new(EvidenceFailureKind::PermissionDenied, "perf_event denied"),
            )],
            report.acs().clone(),
            DiagnosticVerdict::Indeterminate,
            Vec::new(),
            ATTRIBUTION,
        );

        let output = render_p2p_diagnostic_text(&report);
        let details = render_p2p_diagnostic_text_with_options(
            &report,
            DiagnosticTextOptions {
                details: true,
                color: ColorMode::Never,
            },
        );

        assert!(output.contains("host-memory traffic  not counter-qualified"));
        assert!(output.contains("Permission needed:"));
        assert!(!output.contains("permission-denied: perf_event denied"));
        assert!(details.contains("permission-denied: perf_event denied"));
        assert!(output.contains(&format!("sudo {}", absolute_release_binary())));
        assert!(output.contains("diag-p2p --device"));
        assert!(output.contains("--size 2147483648 --samples 50 --warmup 1000000us"));
    }

    #[test]
    fn terminal_counter_failure_is_not_reported_as_available() {
        let base = base_report(DiagnosticVerdict::Indeterminate);
        let failed_phase = CounterPhaseReport::available(
            CounterPhase::DirectUpi,
            1024,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            CounterRunReport::new(
                Duration::from_millis(20),
                Vec::new(),
                Vec::new(),
                Some(EvidenceFailure::other("counter read failed")),
            ),
        );
        let report = P2pDiagnosticReport::new(
            base.source().clone(),
            base.destination().clone(),
            base.peer_access().clone(),
            base.route().clone(),
            base.direct().clone(),
            base.staged().clone(),
            base.cpu().clone(),
            vec![failed_phase],
            base.acs().clone(),
            DiagnosticVerdict::Indeterminate,
            base.reasons().to_vec(),
            ATTRIBUTION,
        );

        let text = render_p2p_diagnostic_text(&report);
        let csv = render_p2p_diagnostic_csv_row(&report);
        let fields = split_csv_record(csv.trim_end());

        assert!(text.contains("optional UPI         failed after 0 samples"));
        assert_eq!(
            csv_field(&fields, "counter_direct_upi_status"),
            "terminal-failure"
        );
    }

    #[test]
    fn sudo_rerun_preserves_non_default_diagnostic_settings() {
        let report = base_report(DiagnosticVerdict::Indeterminate);
        let mut direct = report.direct().clone();
        direct.selected_group = Some(QueueGroupInfo {
            ordinal: 7,
            flags: QueueFlags {
                copy: true,
                compute: false,
            },
            queue_count: 2,
        });
        direct.byte_count = 512 * 1024 * 1024;
        direct.requested_samples = 64;
        direct.warmup = Duration::from_micros(1_500);
        let report = P2pDiagnosticReport::new(
            report.source().clone(),
            report.destination().clone(),
            report.peer_access().clone(),
            report.route().clone(),
            direct,
            report.staged().clone(),
            report.cpu().clone(),
            report.phases().to_vec(),
            report.acs().clone(),
            report.verdict().clone(),
            report.reasons().to_vec(),
            ATTRIBUTION,
        );

        let command = sudo_rerun_command(&report);

        assert!(command.contains("--queue-group 7"));
        assert!(command.contains("--size 536870912"));
        assert!(command.contains("--samples 64"));
        assert!(command.contains("--warmup 1500us"));
    }

    #[cfg(unix)]
    #[test]
    fn reports_acs_extended_config_unavailable_redirect_and_no_redirect() {
        let acs = acs_evidence_with_configs(&[
            config_with_acs(0x000c, 0x000c),
            config_with_acs(0x0020, 0x0000),
            vec![0; 64],
        ]);
        let report = report_with_acs(acs);
        let output = render_p2p_diagnostic_text_with_options(
            &report,
            DiagnosticTextOptions {
                details: true,
                color: ColorMode::Never,
            },
        );

        assert!(output.contains("RR/CR policy enabled with RR/CR bits"));
        assert!(output.contains("RR/CR policy disabled (not proof of direct)"));
        assert!(output.contains("extended config unavailable (64 config bytes"));
        assert!(output.contains("rollup RR/CR policy enabled on 0000:10:01.0"));

        let csv = render_p2p_diagnostic_csv_row(&report);
        let fields = split_csv_record(csv.trim_end());
        assert_eq!(
            csv_field(&fields, "acs_rollup"),
            "RR/CR policy enabled on 0000:10:01.0"
        );
        assert_eq!(csv_field(&fields, "enabled_redirect_bits"), "RR;CR");
        let bridge_evidence = csv_field(&fields, "acs_bridge_evidence");
        assert!(bridge_evidence.contains("ctl=0x000c"));
        assert!(bridge_evidence.contains("extended-config-unavailable:64-bytes"));

        let no_capability = report_with_acs(acs_evidence_with_configs(&[config_without_acs()]));
        let csv = render_p2p_diagnostic_csv_row(&no_capability);
        let fields = split_csv_record(csv.trim_end());
        assert!(csv_field(&fields, "acs_bridge_evidence").contains("no-acs-capability"));
    }

    #[cfg(unix)]
    #[test]
    fn rolls_up_no_redirect_without_calling_it_direct() {
        let acs = acs_evidence_with_configs(&[config_with_acs(0x0010, 0x0010)]);
        let report = report_with_acs(acs);
        let output = render_p2p_diagnostic_text_with_options(
            &report,
            DiagnosticTextOptions {
                details: true,
                color: ColorMode::Never,
            },
        );

        assert!(output.contains(
            "rollup RR/CR policy disabled on readable ACS bridges (not proof of direct)"
        ));
        assert!(!output.contains("rollup direct"));
    }

    #[cfg(unix)]
    #[test]
    fn reports_egress_control_without_calling_it_redirect() {
        let acs = acs_evidence_with_configs(&[config_with_acs(0x0020, 0x0020)]);
        let report = report_with_acs(acs);
        let output = render_p2p_diagnostic_text_with_options(
            &report,
            DiagnosticTextOptions {
                details: true,
                color: ColorMode::Never,
            },
        );

        assert!(output.contains("enabled ACS controls EC"));
        assert!(output.contains("RR/CR policy disabled"));
        assert!(!output.contains("rollup RR/CR policy enabled"));

        let csv = render_p2p_diagnostic_csv_row(&report);
        let fields = split_csv_record(csv.trim_end());
        assert_eq!(
            csv_field(&fields, "acs_rollup"),
            "RR/CR policy disabled on readable ACS bridges (not proof of direct)"
        );
        assert_eq!(csv_field(&fields, "enabled_redirect_bits"), "");
    }

    #[cfg(unix)]
    #[test]
    fn keeps_egress_control_out_of_redirect_bit_list() {
        let acs = acs_evidence_with_configs(&[config_with_acs(0x0024, 0x0024)]);
        let report = report_with_acs(acs);
        let output = render_p2p_diagnostic_text_with_options(
            &report,
            DiagnosticTextOptions {
                details: true,
                color: ColorMode::Never,
            },
        );

        assert!(output.contains("RR/CR policy enabled with RR bits; enabled ACS controls RR/EC"));

        let csv = render_p2p_diagnostic_csv_row(&report);
        let fields = split_csv_record(csv.trim_end());
        assert_eq!(csv_field(&fields, "enabled_redirect_bits"), "RR");
    }

    #[test]
    fn renders_skipped_staged_reason() {
        let mut report = base_report(DiagnosticVerdict::Indeterminate);
        let mut staged = report.staged().clone();
        staged.outcome = CaseOutcome::Skipped {
            reason: "peer access unsupported".to_owned(),
        };
        report = P2pDiagnosticReport::new(
            report.source().clone(),
            report.destination().clone(),
            report.peer_access().clone(),
            report.route().clone(),
            report.direct().clone(),
            staged,
            report.cpu().clone(),
            report.phases().to_vec(),
            report.acs().clone(),
            DiagnosticVerdict::Indeterminate,
            Vec::new(),
            ATTRIBUTION,
        );

        let text = render_p2p_diagnostic_text(&report);
        let details = render_p2p_diagnostic_text_with_options(
            &report,
            DiagnosticTextOptions {
                details: true,
                color: ColorMode::Never,
            },
        );
        let csv = render_p2p_diagnostic_csv(&report);

        assert!(text.contains("skipped (peer access unsupported)"));
        assert!(details.contains("explicit staged skipped: peer access unsupported"));
        assert!(csv.contains(",skipped,,,,,"));
        assert!(csv.contains("staged=skipped (peer access unsupported)"));
    }

    #[test]
    fn separated_clusters_lead_measurement_text_and_suppress_median_marker() {
        let samples = [
            29.0, 29.0, 30.0, 30.0, 31.0, 31.0, 32.0, 32.0, 41.0, 41.0, 41.0, 41.0, 42.0, 42.0,
            42.0, 42.0, 43.0, 43.0, 43.0, 43.0,
        ];
        let summary = stats::summarize(&samples).expect("summary");
        assert!(matches!(
            summary.shape,
            DistributionShape::SeparatedClusters(_)
        ));

        let measurement = render_separated_clusters(&summary, true);
        assert!(measurement.starts_with("separated-cluster candidate"));
        assert!(measurement.contains("median"));
        assert!(measurement.contains("not representative"));
        let utilization = link_utilization_summary(
            &summary,
            &LinkInfo::Known {
                generation: 5,
                width: 16,
                theoretical_gb_s: 63.015_384,
            },
        );
        assert!(utilization.contains("lower"));
        assert!(utilization.contains("upper"));
        assert!(!utilization.contains("median"));

        let mut case = measured_case(
            TransferClass::D2DDirect,
            Operation::Direct {
                peer_access: PeerAccess::Yes,
                route: PeerRoute::Unknown("test".to_owned()),
            },
        );
        case.outcome = CaseOutcome::Measured {
            time_summary: Box::new(summary),
            summary,
            samples_gb_s: samples.to_vec(),
        };
        let mut lines = Vec::new();
        append_case_measurement_compact(&mut lines, "direct", &case);
        assert!(lines[0].contains("overall median omitted"));
        assert!(lines[1].contains("lower"));
        assert!(lines[1].contains("p10..p90"));
        assert!(lines[1].contains("8/20"));
        assert!(lines[2].contains("upper"));
        assert!(lines[2].contains("12/20"));

        let histogram = Histogram::from_samples(&samples, 6).expect("histogram");
        let rows = render_histogram(&histogram, None, ColorMode::Ansi).expect("render");
        assert!(rows.iter().all(|row| !row.contains("median")));
        assert!(rows.iter().all(|row| !row.contains('◆')));
    }

    #[test]
    fn human_text_omits_perfmon_attribution_even_with_details() {
        let report = base_report(DiagnosticVerdict::Indeterminate);
        let concise = render_p2p_diagnostic_text(&report);
        let details = render_p2p_diagnostic_text_with_options(
            &report,
            DiagnosticTextOptions {
                details: true,
                color: ColorMode::Never,
            },
        );

        assert!(!concise.contains("Perfmon events"));
        assert!(!details.contains("Perfmon events"));
        assert!(!details.contains(ATTRIBUTION.upstream_commit));
        assert!(render_p2p_diagnostic_csv(&report).contains(ATTRIBUTION.upstream_commit));
    }

    fn base_report(verdict: DiagnosticVerdict) -> P2pDiagnosticReport {
        let source = device(0, "0000:01:00.0");
        let destination = device(1, "0000:02:00.0");
        P2pDiagnosticReport::new(
            source,
            destination,
            PeerAccess::Yes,
            PeerRoute::SharedUpstreamBridge {
                common_bridge: "0000:00:01.0".to_owned(),
            },
            measured_case(
                TransferClass::D2DDirect,
                Operation::Direct {
                    peer_access: PeerAccess::Yes,
                    route: PeerRoute::SharedUpstreamBridge {
                        common_bridge: "0000:00:01.0".to_owned(),
                    },
                },
            ),
            measured_case(
                TransferClass::D2DStaged,
                Operation::ExplicitStaged {
                    route: PeerRoute::SharedUpstreamBridge {
                        common_bridge: "0000:00:01.0".to_owned(),
                    },
                },
            ),
            CpuEvidence::new(
                EvidenceAvailability::Available(CpuIdentity::new(
                    CpuVendor::GenuineIntel,
                    6,
                    CpuModel::SAPPHIRE_RAPIDS_X,
                )),
                EvidenceAvailability::Available(CpuProfile::SapphireRapidsX),
            ),
            vec![
                available_phase(CounterPhase::ExplicitStagedMemory),
                available_phase(CounterPhase::DirectMemory),
                available_phase(CounterPhase::DirectPeerWrite),
                available_phase(CounterPhase::DirectPeerRead),
                available_phase(CounterPhase::DirectUpi),
            ],
            EvidenceAvailability::Unsupported(EvidenceFailure::new(
                EvidenceFailureKind::Unsupported,
                "ACS unavailable in synthetic report",
            )),
            verdict,
            Vec::new(),
            ATTRIBUTION,
        )
    }

    fn report_with_acs(
        acs: EvidenceAvailability<crate::evidence::AcsBridgePathEvidence>,
    ) -> P2pDiagnosticReport {
        let report = base_report(DiagnosticVerdict::CounterConsistentPeer {
            route_qualifier: None,
        });
        P2pDiagnosticReport::new(
            report.source().clone(),
            report.destination().clone(),
            report.peer_access().clone(),
            report.route().clone(),
            report.direct().clone(),
            report.staged().clone(),
            report.cpu().clone(),
            report.phases().to_vec(),
            acs,
            report.verdict().clone(),
            report.reasons().to_vec(),
            ATTRIBUTION,
        )
    }

    fn device(index: u32, bdf: &str) -> DeviceInfo {
        DeviceInfo {
            index,
            name: "Test GPU".to_owned(),
            pci_address: Some(bdf.to_owned()),
            pcie_link: LinkInfo::Known {
                generation: 5,
                width: 16,
                theoretical_gb_s: 63.015_384,
            },
            queue_groups: vec![QueueGroupInfo {
                ordinal: 0,
                flags: QueueFlags {
                    copy: true,
                    compute: true,
                },
                queue_count: 1,
            }],
        }
    }

    fn measured_case(transfer_class: TransferClass, operation: Operation) -> BenchCase {
        let samples = vec![40.0, 41.0, 42.0, 43.0, 44.0, 45.0, 46.0, 47.0, 48.0, 49.0];
        let summary = stats::summarize(&samples).expect("summary");
        BenchCase {
            mode: BenchMode::Saturation,
            selected_group: None,
            streams: vec![QueueStreamInfo {
                group_ordinal: 0,
                queue_index: 0,
                flags: QueueFlags {
                    copy: true,
                    compute: true,
                },
            }],
            second_phase_streams: Vec::new(),
            verification_stream: None,
            transfer_class,
            operation,
            source: Endpoint::Device(0),
            destination: Endpoint::Device(1),
            byte_count: 2 * 1024 * 1024 * 1024,
            allocation: AllocationKind::Device,
            timing: TimingMode::WallClock,
            warmup: Duration::from_secs(1),
            requested_samples: 50,
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

    fn available_phase(phase: CounterPhase) -> CounterPhaseReport {
        let role = match phase {
            CounterPhase::ExplicitStagedMemory | CounterPhase::DirectMemory => {
                EventRole::IioDataReqOfCpuMemReadAllParts
            }
            CounterPhase::DirectPeerWrite => EventRole::IioDataReqOfCpuPeerWriteAllParts,
            CounterPhase::DirectPeerRead => EventRole::IioDataReqOfCpuPeerReadAllParts,
            CounterPhase::DirectUpi => EventRole::UpiTxDataFlitsAll,
        };
        CounterPhaseReport::available(
            phase,
            1024,
            vec![role],
            vec![CounterGroupReport::new("uncore_iio_0", 0, Vec::new())],
            Vec::new(),
            CounterRunReport::new(
                Duration::from_millis(10),
                vec![counter_window(role, 10), counter_window(role, 11)],
                vec![
                    CounterSample::new(0, counter_window(role, 100)),
                    CounterSample::new(1, counter_window(role, 150)),
                    CounterSample::new(2, counter_window(role, 200)),
                ],
                None,
            ),
        )
    }

    fn counter_window(role: EventRole, value: u64) -> CounterWindow {
        let snapshot = CounterSnapshot::new(EvidenceRunId::new(1), Instant::now(), Vec::new())
            .expect("snapshot");
        CounterWindow::new(
            snapshot.clone(),
            snapshot,
            Duration::from_millis(1),
            Vec::new(),
            vec![RoleCounterTotal::new(role, value)],
        )
    }

    #[cfg(unix)]
    fn acs_evidence_with_configs(
        configs: &[Vec<u8>],
    ) -> EvidenceAvailability<crate::evidence::AcsBridgePathEvidence> {
        let sysfs = TestSysfs::new();
        let root = PciAddress::new(0, 0x10, 1, 0).expect("root");
        let source_port = PciAddress::new(0, 0x11, 1, 0).expect("source port");
        let destination_port = PciAddress::new(0, 0x11, 2, 0).expect("destination port");
        let source = PciAddress::new(0, 0x12, 0, 0).expect("source");
        let destination = PciAddress::new(0, 0x13, 0, 0).expect("destination");
        let source_chain = sysfs.add_nested_device(source, &[root, source_port, source]);
        let destination_chain =
            sysfs.add_nested_device(destination, &[root, destination_port, destination]);
        let bridge_paths = [
            source_chain[0].1.clone(),
            source_chain[1].1.clone(),
            destination_chain[1].1.clone(),
        ];
        for (path, config) in bridge_paths.iter().zip(configs) {
            write_config(path, config);
        }
        for path in bridge_paths.iter().skip(configs.len()) {
            write_config(path, &config_without_acs());
        }

        EvidenceAvailability::Available(
            read_acs_bridge_path_from_sysfs(&sysfs.root, source, destination)
                .expect("read ACS evidence"),
        )
    }

    fn config_without_acs() -> Vec<u8> {
        let mut config = vec![0; PCI_EXT_CAP_START + 4];
        write_ext_header(&mut config, PCI_EXT_CAP_START, 0x0001, 1, 0);
        config
    }

    fn config_with_acs(raw_capability: u16, raw_control: u16) -> Vec<u8> {
        let mut config = vec![0; PCI_EXT_CAP_START + ACS_BODY_LEN];
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
            let root = std::env::temp_dir().join(format!(
                "xfer-output-diagnostics-test-{}-{nonce}",
                std::process::id()
            ));
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

    fn csv_field<'fields>(fields: &'fields [String], column: &str) -> &'fields str {
        let index = P2P_DIAGNOSTIC_CSV_HEADER
            .split(',')
            .position(|candidate| candidate == column)
            .expect("diagnostic CSV column exists");
        &fields[index]
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
