use crate::evidence::intel_perfmon::EventRole;
use crate::output::{BenchCase, CaseOutcome, DeviceInfo, LinkInfo};
use crate::stats::{DistributionShape, Summary};

use super::model::{
    CounterPhase, CounterPhaseReport, CounterRunReport, CounterWindow, DiagnosticVerdict,
    EvidenceFailureKind, EvidenceReason, EvidenceReasonCode, LikelyMechanism, PeerRouteQualifier,
};

const MIN_VALID_COUNTER_SAMPLES: usize = 5;
const COUNTER_UNITS_PER_BYTE_DENOMINATOR: u64 = 4;
const ABSOLUTE_GATE_NUMERATOR: u128 = 1;
const ABSOLUTE_GATE_DENOMINATOR: u128 = 2;
const NOISE_GATE_NUMERATOR: u128 = 1;
const NOISE_GATE_DENOMINATOR: u128 = 20;
const DIRECT_TO_STAGED_MEMORY_NUMERATOR: u128 = 1;
const DIRECT_TO_STAGED_MEMORY_DENOMINATOR: u128 = 2;
const NEGATIVE_GATE_NUMERATOR: u128 = 1;
const NEGATIVE_GATE_DENOMINATOR: u128 = 20;

pub(crate) struct VerdictInput<'a> {
    pub(crate) cpu_supported: bool,
    pub(crate) source: &'a DeviceInfo,
    pub(crate) destination: &'a DeviceInfo,
    pub(crate) direct: &'a BenchCase,
    pub(crate) staged: &'a BenchCase,
    pub(crate) phases: &'a [CounterPhaseReport],
    pub(crate) acs_redirect: bool,
    pub(crate) acs_state_changed: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct VerdictAnalysis {
    pub(crate) verdict: DiagnosticVerdict,
    pub(crate) reasons: Vec<EvidenceReason>,
}

pub(crate) fn synthesize(input: &VerdictInput<'_>) -> VerdictAnalysis {
    let mut reasons = Vec::new();
    append_distribution_reasons(input, &mut reasons);
    if input.acs_state_changed {
        reasons.push(EvidenceReason::new(
            EvidenceReasonCode::AcsStateChanged,
            "ACS bridge evidence changed between the preflight and final snapshots; final values are shown and ACS does not qualify the verdict",
        ));
    }
    if !input.cpu_supported {
        reasons.push(EvidenceReason::new(
            EvidenceReasonCode::CpuUnsupported,
            "counter-consistent verdicts require SPR-X, GNR-X, or GNR-D CPU identity/profile",
        ));
        return heuristic_verdict(input, reasons);
    }
    if let Some(reason) = verdict_terminal_failure(input.phases) {
        reasons.push(reason);
        return heuristic_verdict(input, reasons);
    }

    let staged = phase(input.phases, CounterPhase::ExplicitStagedMemory);
    let direct = phase(input.phases, CounterPhase::DirectMemory);
    let staged_read = gated_role(
        staged,
        EventRole::IioDataReqOfCpuMemReadAllParts,
        &mut reasons,
    );
    let staged_write = gated_role(
        staged,
        EventRole::IioDataReqOfCpuMemWriteAllParts,
        &mut reasons,
    );
    let memory_calibrated = staged_read.passes && staged_write.passes;
    if !memory_calibrated {
        reasons.push(EvidenceReason::new(
            EvidenceReasonCode::StagedCalibrationMissing,
            "explicit staged MEM_READ and MEM_WRITE did not both pass absolute/noise gates",
        ));
    }

    let direct_memory = if memory_calibrated {
        let direct_read = gated_role(
            direct,
            EventRole::IioDataReqOfCpuMemReadAllParts,
            &mut reasons,
        );
        let direct_write = gated_role(
            direct,
            EventRole::IioDataReqOfCpuMemWriteAllParts,
            &mut reasons,
        );
        let present = direct_read.passes
            && direct_write.passes
            && ratio_gate(
                direct_read.p10,
                staged_read.median,
                EventRole::IioDataReqOfCpuMemReadAllParts,
                &mut reasons,
            )
            && ratio_gate(
                direct_write.p10,
                staged_write.median,
                EventRole::IioDataReqOfCpuMemWriteAllParts,
                &mut reasons,
            );
        if present {
            MemorySignal::Present
        } else if direct_read.valid
            && direct_write.valid
            && direct_read.below_negative_gate
            && direct_write.below_negative_gate
        {
            MemorySignal::Absent
        } else {
            MemorySignal::Unknown
        }
    } else {
        MemorySignal::Unknown
    };

    let peer_signal = peer_signal(input.phases, &mut reasons);
    let route_qualifier = input
        .acs_redirect
        .then_some(PeerRouteQualifier::AcsRedirectedOrUpstreamRoutedPeerTrafficNotHostBounce);
    if input.acs_redirect && peer_signal {
        reasons.push(EvidenceReason::new(
            EvidenceReasonCode::AcsRedirectObserved,
            "ACS RR/CR policy was enabled on at least one bridge; peer counter signal is qualified as redirected/upstream-routed peer traffic, not host bounce",
        ));
    }

    let verdict = match (direct_memory, peer_signal) {
        (MemorySignal::Present, true) => {
            DiagnosticVerdict::MixedSignalsAcrossRuns { route_qualifier }
        }
        (MemorySignal::Present, false) => DiagnosticVerdict::CounterConsistentHostBounce,
        (MemorySignal::Absent, true) => {
            DiagnosticVerdict::CounterConsistentPeer { route_qualifier }
        }
        (MemorySignal::Absent | MemorySignal::Unknown, false) | (MemorySignal::Unknown, true) => {
            return heuristic_verdict(input, reasons);
        }
    };

    VerdictAnalysis { verdict, reasons }
}

fn verdict_terminal_failure(phases: &[CounterPhaseReport]) -> Option<EvidenceReason> {
    phases
        .iter()
        .filter(|phase| phase.phase() != CounterPhase::DirectUpi)
        .find_map(|phase| {
            let failure = phase.available_run()?.terminal_failure()?;
            let code = match failure.kind() {
                EvidenceFailureKind::CounterOverflow => EvidenceReasonCode::CounterOverflow,
                EvidenceFailureKind::TopologyMismatch => {
                    EvidenceReasonCode::CounterTopologyMismatch
                }
                EvidenceFailureKind::PermissionDenied
                | EvidenceFailureKind::Unsupported
                | EvidenceFailureKind::Malformed
                | EvidenceFailureKind::ResourceUnavailable
                | EvidenceFailureKind::Io
                | EvidenceFailureKind::Other => EvidenceReasonCode::CounterTerminalFailure,
            };
            Some(EvidenceReason::new(
                code,
                format!(
                    "{} terminal counter failure disqualifies counter-consistent verdicts: {}",
                    phase.phase(),
                    failure.message()
                ),
            ))
        })
}

fn phase(phases: &[CounterPhaseReport], target: CounterPhase) -> Option<&CounterPhaseReport> {
    phases.iter().find(|phase| phase.phase() == target)
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct GateResult {
    valid: bool,
    passes: bool,
    below_negative_gate: bool,
    p10: u128,
    median: u128,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MemorySignal {
    Present,
    Absent,
    Unknown,
}

fn gated_role(
    phase: Option<&CounterPhaseReport>,
    role: EventRole,
    reasons: &mut Vec<EvidenceReason>,
) -> GateResult {
    let Some(phase) = phase else {
        reasons.push(EvidenceReason::new(
            EvidenceReasonCode::CounterEvidenceUnavailable,
            format!("missing counter phase for {role}"),
        ));
        return GateResult::default();
    };
    let Some(run) = phase.available_run() else {
        reasons.push(unavailable_phase_reason(phase, role));
        return GateResult::default();
    };
    if let Some(failure) = run.terminal_failure() {
        let code = match failure.kind() {
            EvidenceFailureKind::CounterOverflow => EvidenceReasonCode::CounterOverflow,
            EvidenceFailureKind::TopologyMismatch => EvidenceReasonCode::CounterTopologyMismatch,
            EvidenceFailureKind::PermissionDenied
            | EvidenceFailureKind::Unsupported
            | EvidenceFailureKind::Malformed
            | EvidenceFailureKind::ResourceUnavailable
            | EvidenceFailureKind::Io
            | EvidenceFailureKind::Other => EvidenceReasonCode::CounterTerminalFailure,
        };
        reasons.push(EvidenceReason::new(
            code,
            format!(
                "{} terminal counter failure disqualifies {role}: {}",
                phase.phase(),
                failure.message()
            ),
        ));
        return GateResult::default();
    }
    let expected = expected_counter_units(phase.expected_transfer_bytes());
    match role_series(run, role) {
        Ok(series) => {
            let baseline = match baseline_series(run, role) {
                Ok(baseline) => baseline,
                Err(reason) => {
                    reasons.push(reason);
                    return GateResult::default();
                }
            };
            if series.len() < MIN_VALID_COUNTER_SAMPLES {
                reasons.push(EvidenceReason::new(
                    EvidenceReasonCode::InsufficientCounterRepeats,
                    format!("{role} has {} valid samples, need at least 5", series.len()),
                ));
                return GateResult::default();
            }
            if baseline.len() < MIN_VALID_COUNTER_SAMPLES {
                reasons.push(EvidenceReason::new(
                    EvidenceReasonCode::InsufficientCounterRepeats,
                    format!("{role} has {} idle baselines, need 5", baseline.len()),
                ));
                return GateResult::default();
            }
            let p10 = percentile_nearest(&series, 10);
            let p90 = percentile_nearest(&series, 90);
            let median = percentile_nearest(&series, 50);
            let baseline_p95 = percentile_nearest(&baseline, 95);
            let (absolute, noise) = match signal_gates(p10, baseline_p95, expected, role) {
                Ok(gates) => gates,
                Err(reason) => {
                    reasons.push(reason);
                    return GateResult::default();
                }
            };
            if !(absolute && noise) {
                reasons.push(EvidenceReason::new(
                    EvidenceReasonCode::CounterSignalBelowGate,
                    format!(
                        "{role} p10 {p10} did not clear absolute/noise gates over idle baseline p95 {baseline_p95}"
                    ),
                ));
            }
            let below_negative_gate = match negative_signal_gate(p90, baseline_p95, expected, role)
            {
                Ok(below) => below,
                Err(reason) => {
                    reasons.push(reason);
                    false
                }
            };
            GateResult {
                valid: true,
                passes: absolute && noise,
                below_negative_gate,
                p10,
                median,
            }
        }
        Err(reason) => {
            reasons.push(reason);
            GateResult::default()
        }
    }
}

fn negative_signal_gate(
    p90: u128,
    baseline_p95: u128,
    expected: u128,
    role: EventRole,
) -> Result<bool, EvidenceReason> {
    let baseline_left = baseline_p95.checked_mul(NEGATIVE_GATE_DENOMINATOR);
    let baseline_right = expected.checked_mul(NEGATIVE_GATE_NUMERATOR);
    let signal_left = p90.checked_mul(NEGATIVE_GATE_DENOMINATOR);
    let signal_right = baseline_p95
        .checked_mul(NEGATIVE_GATE_DENOMINATOR)
        .and_then(|baseline| {
            expected
                .checked_mul(NEGATIVE_GATE_NUMERATOR)
                .and_then(|margin| baseline.checked_add(margin))
        });
    match (baseline_left, baseline_right, signal_left, signal_right) {
        (Some(baseline_left), Some(baseline_right), Some(signal_left), Some(signal_right)) => {
            Ok(baseline_left <= baseline_right && signal_left <= signal_right)
        }
        _ => Err(EvidenceReason::new(
            EvidenceReasonCode::CounterOverflow,
            format!("integer overflow while evaluating negative signal gate for {role}"),
        )),
    }
}

fn unavailable_phase_reason(phase: &CounterPhaseReport, role: EventRole) -> EvidenceReason {
    let Some(failure) = phase.evidence().failure() else {
        return EvidenceReason::new(
            EvidenceReasonCode::CounterEvidenceUnavailable,
            format!("{} unavailable for {role}", phase.phase()),
        );
    };
    let code = match failure.kind() {
        EvidenceFailureKind::CounterOverflow => EvidenceReasonCode::CounterOverflow,
        EvidenceFailureKind::TopologyMismatch => EvidenceReasonCode::CounterTopologyMismatch,
        EvidenceFailureKind::PermissionDenied
        | EvidenceFailureKind::Unsupported
        | EvidenceFailureKind::Malformed
        | EvidenceFailureKind::ResourceUnavailable
        | EvidenceFailureKind::Io
        | EvidenceFailureKind::Other => EvidenceReasonCode::CounterEvidenceUnavailable,
    };
    EvidenceReason::new(
        code,
        format!(
            "{} unavailable for {role}: {}",
            phase.phase(),
            failure.message()
        ),
    )
}

fn signal_gates(
    p10: u128,
    baseline_p95: u128,
    expected: u128,
    role: EventRole,
) -> Result<(bool, bool), EvidenceReason> {
    let absolute_left = p10.checked_mul(ABSOLUTE_GATE_DENOMINATOR);
    let absolute_right = expected.checked_mul(ABSOLUTE_GATE_NUMERATOR);
    let noise_left = p10.checked_mul(NOISE_GATE_DENOMINATOR);
    let noise_right = baseline_p95
        .checked_mul(NOISE_GATE_DENOMINATOR)
        .and_then(|baseline| {
            expected
                .checked_mul(NOISE_GATE_NUMERATOR)
                .and_then(|margin| baseline.checked_add(margin))
        });
    match (absolute_left, absolute_right, noise_left, noise_right) {
        (Some(absolute_left), Some(absolute_right), Some(noise_left), Some(noise_right)) => {
            Ok((absolute_left >= absolute_right, noise_left >= noise_right))
        }
        _ => Err(EvidenceReason::new(
            EvidenceReasonCode::CounterOverflow,
            format!("integer overflow while evaluating signal gates for {role}"),
        )),
    }
}

fn ratio_gate(
    direct_p10: u128,
    staged_median: u128,
    role: EventRole,
    reasons: &mut Vec<EvidenceReason>,
) -> bool {
    let direct = direct_p10.checked_mul(DIRECT_TO_STAGED_MEMORY_DENOMINATOR);
    let staged = staged_median.checked_mul(DIRECT_TO_STAGED_MEMORY_NUMERATOR);
    if let (Some(direct), Some(staged)) = (direct, staged) {
        direct >= staged
    } else {
        reasons.push(EvidenceReason::new(
            EvidenceReasonCode::CounterOverflow,
            format!("integer overflow while comparing direct and staged {role} counters"),
        ));
        false
    }
}

fn peer_signal(phases: &[CounterPhaseReport], reasons: &mut Vec<EvidenceReason>) -> bool {
    let mut available_role = false;
    let mut signal = false;
    for (phase_id, roles) in [
        (
            CounterPhase::DirectPeerWrite,
            [
                EventRole::IioDataReqOfCpuPeerWriteAllParts,
                EventRole::IioDataReqByCpuPeerWriteAllParts,
            ],
        ),
        (
            CounterPhase::DirectPeerRead,
            [
                EventRole::IioDataReqOfCpuPeerReadAllParts,
                EventRole::IioDataReqByCpuPeerReadAllParts,
            ],
        ),
    ] {
        let phase = phase(phases, phase_id);
        for role in roles {
            if phase.is_some_and(|phase| phase.roles().contains(&role)) {
                available_role = true;
                signal |= gated_role(phase, role, reasons).passes;
            }
        }
    }

    if !available_role {
        reasons.push(EvidenceReason::new(
            EvidenceReasonCode::CounterEvidenceUnavailable,
            "no peer read/write counter role was available",
        ));
    }
    signal
}

fn role_series(run: &CounterRunReport, role: EventRole) -> Result<Vec<u128>, EvidenceReason> {
    let mut values = Vec::new();
    for sample in run.sample_windows() {
        let window = sample.window();
        if !window_non_multiplexed(window) {
            return Err(EvidenceReason::new(
                EvidenceReasonCode::CounterMultiplexed,
                format!("{role} counter phase used multiplexed or zero-running counters"),
            ));
        }
        if let Some(value) = window.role_total(role) {
            values.push(u128::from(value));
        }
    }
    Ok(values)
}

fn baseline_series(run: &CounterRunReport, role: EventRole) -> Result<Vec<u128>, EvidenceReason> {
    let Some(longest_sample_ns) = run
        .sample_windows()
        .iter()
        .map(|sample| sample.window().elapsed().as_nanos())
        .max()
        .filter(|duration| *duration > 0)
    else {
        return Ok(Vec::new());
    };

    run.baseline_windows()
        .iter()
        .filter(|window| window_non_multiplexed(window))
        .filter_map(|window| window.role_total(role).map(|value| (window, value)))
        .map(|(window, value)| {
            let baseline_ns = window.elapsed().as_nanos();
            if baseline_ns == 0 {
                return Err(EvidenceReason::new(
                    EvidenceReasonCode::CounterEvidenceUnavailable,
                    format!("{role} idle baseline has a zero-duration counter window"),
                ));
            }
            // Never scale an idle count downward. Shorter copy windows therefore
            // retain the full 20 ms baseline count, while longer windows use a
            // ceil-rounded extrapolation to the longest measured copy window.
            let target_ns = longest_sample_ns.max(baseline_ns);
            checked_ceil_mul_div(u128::from(value), target_ns, baseline_ns).ok_or_else(|| {
                EvidenceReason::new(
                    EvidenceReasonCode::CounterOverflow,
                    format!("integer overflow while scaling {role} idle baseline"),
                )
            })
        })
        .collect()
}

fn checked_ceil_mul_div(value: u128, multiplier: u128, divisor: u128) -> Option<u128> {
    if divisor == 0 {
        return None;
    }
    let product = value.checked_mul(multiplier)?;
    let quotient = product / divisor;
    quotient.checked_add(u128::from(product % divisor != 0))
}

fn window_non_multiplexed(window: &CounterWindow) -> bool {
    window.deltas().iter().all(|delta| {
        !delta.time_running().is_zero() && delta.time_enabled() == delta.time_running()
    })
}

fn percentile_nearest(values: &[u128], percentile: u32) -> u128 {
    debug_assert!(!values.is_empty());
    debug_assert!(percentile <= 100);
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    if percentile == 0 {
        return sorted[0];
    }
    let rank = ((sorted.len() as u32)
        .saturating_mul(percentile)
        .saturating_add(99)
        / 100)
        .max(1);
    sorted[(rank as usize).saturating_sub(1).min(sorted.len() - 1)]
}

fn expected_counter_units(bytes: u64) -> u128 {
    u128::from((bytes / COUNTER_UNITS_PER_BYTE_DENOMINATOR).max(1))
}

fn heuristic_verdict(
    input: &VerdictInput<'_>,
    mut reasons: Vec<EvidenceReason>,
) -> VerdictAnalysis {
    let Some(direct) = measured_summary(input.direct) else {
        reasons.push(EvidenceReason::new(
            EvidenceReasonCode::HeuristicUnavailable,
            "direct benchmark summary is unavailable",
        ));
        return VerdictAnalysis {
            verdict: DiagnosticVerdict::Indeterminate,
            reasons,
        };
    };
    let staged = measured_summary(input.staged);

    if !matches!(direct.summary.shape, DistributionShape::Ordinary)
        || staged.is_some_and(|staged| !matches!(staged.summary.shape, DistributionShape::Ordinary))
    {
        reasons.push(EvidenceReason::new(
            EvidenceReasonCode::HeuristicUnavailable,
            "median-based transport heuristics are disabled for separated throughput clusters",
        ));
        return VerdictAnalysis {
            verdict: DiagnosticVerdict::Indeterminate,
            reasons,
        };
    }

    if staged.is_some_and(|staged| approximately_staged(direct, staged)) {
        reasons.push(EvidenceReason::new(
            EvidenceReasonCode::HeuristicHostStaged,
            "direct and explicit staged medians have overlapping CIs, bounded MAD, and bounded ratio",
        ));
        return VerdictAnalysis {
            verdict: DiagnosticVerdict::HeuristicOnly {
                likely: LikelyMechanism::HostStaged,
            },
            reasons,
        };
    }

    if let Some(link) = min_endpoint_link(input.source, input.destination) {
        if link.low
            && direct.summary.median >= 0.5 * link.gb_s
            && direct.summary.median <= 1.1 * link.gb_s
        {
            reasons.push(EvidenceReason::new(
                EvidenceReasonCode::HeuristicLinkLimited,
                "direct bandwidth is near a low negotiated endpoint PCIe link",
            ));
            return VerdictAnalysis {
                verdict: DiagnosticVerdict::HeuristicOnly {
                    likely: LikelyMechanism::LinkLimited,
                },
                reasons,
            };
        }
        if direct.summary.median >= 0.6 * link.gb_s {
            reasons.push(EvidenceReason::new(
                EvidenceReasonCode::HeuristicDeviceSide,
                "direct bandwidth reaches at least 60% of the minimum endpoint negotiated link",
            ));
            return VerdictAnalysis {
                verdict: DiagnosticVerdict::HeuristicOnly {
                    likely: LikelyMechanism::DeviceSide,
                },
                reasons,
            };
        }
    }

    if staged.is_some_and(|staged| clearly_faster_than_staged(direct, staged)) {
        reasons.push(EvidenceReason::new(
            EvidenceReasonCode::HeuristicDeviceSide,
            "direct bandwidth is clearly faster than explicit staged calibration",
        ));
        return VerdictAnalysis {
            verdict: DiagnosticVerdict::HeuristicOnly {
                likely: LikelyMechanism::DeviceSide,
            },
            reasons,
        };
    }

    let message = if staged.is_none() {
        "explicit staged benchmark summary is unavailable and direct-only heuristics did not identify a likely mechanism"
    } else {
        "counter evidence and conservative throughput heuristics did not identify a likely mechanism"
    };
    reasons.push(EvidenceReason::new(
        EvidenceReasonCode::HeuristicUnavailable,
        message,
    ));
    VerdictAnalysis {
        verdict: DiagnosticVerdict::Indeterminate,
        reasons,
    }
}

fn append_distribution_reasons(input: &VerdictInput<'_>, reasons: &mut Vec<EvidenceReason>) {
    for (label, case) in [("direct", input.direct), ("explicit staged", input.staged)] {
        let Some(measured) = measured_summary(case) else {
            continue;
        };
        let DistributionShape::SeparatedClusters(clusters) = measured.summary.shape else {
            continue;
        };
        reasons.push(EvidenceReason::new(
            EvidenceReasonCode::ThroughputSeparatedClusters,
            format!(
                "{label} throughput has separated-cluster candidate centers near {:.2} GB/s ({}) and {:.2} GB/s ({}); possible causes include scheduling, clocks/power, contention, or route changes",
                clusters.lower_center,
                clusters.lower_count,
                clusters.upper_center,
                clusters.upper_count
            ),
        ));
    }
}

#[derive(Clone, Copy)]
struct MeasuredCase<'a> {
    summary: &'a Summary,
}

fn measured_summary(case: &BenchCase) -> Option<MeasuredCase<'_>> {
    match &case.outcome {
        CaseOutcome::Measured { summary, .. } => Some(MeasuredCase { summary }),
        CaseOutcome::Skipped { .. } => None,
    }
}

fn approximately_staged(direct: MeasuredCase<'_>, staged: MeasuredCase<'_>) -> bool {
    let direct_ci = direct.summary.median_confidence;
    let staged_ci = staged.summary.median_confidence;
    let ci_overlap = direct_ci.lower_bound <= staged_ci.upper_bound
        && staged_ci.lower_bound <= direct_ci.upper_bound;
    let direct_mad_ok = direct.summary.mad <= 0.15 * direct.summary.median.max(f64::EPSILON);
    let staged_mad_ok = staged.summary.mad <= 0.15 * staged.summary.median.max(f64::EPSILON);
    let ratio = direct.summary.median / staged.summary.median.max(f64::EPSILON);
    ci_overlap && direct_mad_ok && staged_mad_ok && (0.8..=1.25).contains(&ratio)
}

fn clearly_faster_than_staged(direct: MeasuredCase<'_>, staged: MeasuredCase<'_>) -> bool {
    direct.summary.median_confidence.lower_bound > staged.summary.median_confidence.upper_bound
        && direct.summary.median >= 1.2 * staged.summary.median
}

#[derive(Clone, Copy)]
struct EndpointLink {
    gb_s: f64,
    low: bool,
}

fn min_endpoint_link(source: &DeviceInfo, destination: &DeviceInfo) -> Option<EndpointLink> {
    let source = endpoint_link(&source.pcie_link)?;
    let destination = endpoint_link(&destination.pcie_link)?;
    Some(if source.gb_s <= destination.gb_s {
        source
    } else {
        destination
    })
}

fn endpoint_link(link: &LinkInfo) -> Option<EndpointLink> {
    match link {
        LinkInfo::Known {
            generation,
            width,
            theoretical_gb_s,
        } => Some(EndpointLink {
            gb_s: *theoretical_gb_s,
            low: *generation < 5 || *width < 16,
        }),
        LinkInfo::Unknown { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use crate::cli::{BenchMode, TimingMode, TransferClass};
    use crate::evidence::counters::{
        CounterId, CounterReading, CounterSnapshot, EvidenceRunId, diff_snapshots,
    };
    use crate::output::{
        AllocationKind, CaseOutcome, Endpoint, Operation, QueueFlags, QueueGroupInfo,
    };
    use crate::stats::{
        ConfidenceInterval, DistributionShape, Quartiles, SeparatedClusters, TukeyFences,
        TukeyOutlierCounts, TukeyOutliers,
    };

    use super::super::model::{
        CounterRunReport, CounterSample, CounterWindow, EvidenceFailure, RoleCounterTotal,
    };
    use super::*;

    fn summary(median: f64, ci_low: f64, ci_high: f64, mad: f64) -> Summary {
        Summary {
            count: 10,
            median,
            median_confidence: ConfidenceInterval {
                confidence_level: 0.95,
                lower_bound: ci_low,
                upper_bound: ci_high,
                resamples: 10,
            },
            mad,
            p5: median,
            p95: median,
            quartiles: Quartiles {
                q1: median,
                q2: median,
                q3: median,
            },
            outliers: TukeyOutliers {
                counts: TukeyOutlierCounts { mild: 0, severe: 0 },
                fences: TukeyFences {
                    mild_lower: median,
                    mild_upper: median,
                    severe_lower: median,
                    severe_upper: median,
                },
            },
            shape: DistributionShape::Ordinary,
        }
    }

    fn bench_case(class: TransferClass, median: f64) -> BenchCase {
        BenchCase {
            mode: BenchMode::Saturation,
            selected_group: None,
            streams: Vec::new(),
            second_phase_streams: Vec::new(),
            verification_stream: None,
            transfer_class: class,
            operation: Operation::Direct {
                peer_access: crate::output::PeerAccess::Yes,
                route: crate::output::PeerRoute::Unknown("test".to_owned()),
            },
            source: Endpoint::Device(0),
            destination: Endpoint::Device(1),
            byte_count: 4096,
            allocation: AllocationKind::Device,
            timing: TimingMode::WallClock,
            warmup: Duration::ZERO,
            requested_samples: 10,
            pcie_link: LinkInfo::Unknown {
                reason: "test".to_owned(),
            },
            outcome: CaseOutcome::Measured {
                time_summary: Box::new(summary(1.0, 1.0, 1.0, 0.0)),
                summary: summary(median, median * 0.95, median * 1.05, median * 0.02),
                samples_gb_s: vec![median; 10],
            },
        }
    }

    fn device(index: u32, generation: u8, width: u16, gb_s: f64) -> DeviceInfo {
        DeviceInfo {
            index,
            name: format!("dev{index}"),
            pci_address: Some(format!("0000:00:0{index}.0")),
            pcie_link: LinkInfo::Known {
                generation,
                width,
                theoretical_gb_s: gb_s,
            },
            queue_groups: vec![QueueGroupInfo {
                ordinal: 0,
                flags: QueueFlags {
                    copy: true,
                    compute: false,
                },
                queue_count: 1,
            }],
        }
    }

    fn phase(
        phase: CounterPhase,
        bytes: u64,
        role_values: &[(EventRole, u64)],
    ) -> CounterPhaseReport {
        let roles = role_values
            .iter()
            .map(|(role, _)| *role)
            .collect::<Vec<_>>();
        let baseline = (0..5)
            .map(|_| synthetic_window(&roles, 0, false))
            .collect::<Vec<_>>();
        let samples = (0..5)
            .map(|index| {
                CounterSample::new(index, synthetic_window_with_values(role_values, false))
            })
            .collect::<Vec<_>>();
        CounterPhaseReport::available(
            phase,
            bytes,
            roles,
            Vec::new(),
            Vec::new(),
            CounterRunReport::new(Duration::from_millis(20), baseline, samples, None),
        )
    }

    fn phase_with_terminal_failure(
        phase: CounterPhase,
        bytes: u64,
        role_values: &[(EventRole, u64)],
    ) -> CounterPhaseReport {
        let roles = role_values
            .iter()
            .map(|(role, _)| *role)
            .collect::<Vec<_>>();
        let baseline = (0..5)
            .map(|_| synthetic_window(&roles, 0, false))
            .collect::<Vec<_>>();
        let samples = (0..5)
            .map(|index| {
                CounterSample::new(index, synthetic_window_with_values(role_values, false))
            })
            .collect::<Vec<_>>();
        CounterPhaseReport::available(
            phase,
            bytes,
            roles,
            Vec::new(),
            Vec::new(),
            CounterRunReport::new(
                Duration::from_millis(20),
                baseline,
                samples,
                Some(EvidenceFailure::other("terminal read failure")),
            ),
        )
    }

    fn multiplexed_phase(
        phase: CounterPhase,
        bytes: u64,
        role_values: &[(EventRole, u64)],
    ) -> CounterPhaseReport {
        let roles = role_values
            .iter()
            .map(|(role, _)| *role)
            .collect::<Vec<_>>();
        let baseline = (0..5)
            .map(|_| synthetic_window(&roles, 0, false))
            .collect::<Vec<_>>();
        let samples = (0..5)
            .map(|index| CounterSample::new(index, synthetic_window_with_values(role_values, true)))
            .collect::<Vec<_>>();
        CounterPhaseReport::available(
            phase,
            bytes,
            roles,
            Vec::new(),
            Vec::new(),
            CounterRunReport::new(Duration::from_millis(20), baseline, samples, None),
        )
    }

    fn insufficient_phase(
        phase: CounterPhase,
        bytes: u64,
        role_values: &[(EventRole, u64)],
    ) -> CounterPhaseReport {
        let roles = role_values
            .iter()
            .map(|(role, _)| *role)
            .collect::<Vec<_>>();
        let baseline = (0..5)
            .map(|_| synthetic_window(&roles, 0, false))
            .collect::<Vec<_>>();
        let samples = (0..4)
            .map(|index| {
                CounterSample::new(index, synthetic_window_with_values(role_values, false))
            })
            .collect::<Vec<_>>();
        CounterPhaseReport::available(
            phase,
            bytes,
            roles,
            Vec::new(),
            Vec::new(),
            CounterRunReport::new(Duration::from_millis(20), baseline, samples, None),
        )
    }

    fn synthetic_window(roles: &[EventRole], value: u64, multiplexed: bool) -> CounterWindow {
        let values = roles.iter().map(|role| (*role, value)).collect::<Vec<_>>();
        synthetic_window_with_values(&values, multiplexed)
    }

    fn synthetic_window_with_values(
        values: &[(EventRole, u64)],
        multiplexed: bool,
    ) -> CounterWindow {
        synthetic_window_with_duration(values, multiplexed, Duration::from_millis(1))
    }

    fn synthetic_window_with_duration(
        values: &[(EventRole, u64)],
        multiplexed: bool,
        duration: Duration,
    ) -> CounterWindow {
        let millis = u64::try_from(duration.as_millis()).expect("test duration fits u64");
        let base = Instant::now();
        let before = snapshot(base, 0, values.iter().map(|(role, _)| (*role, 0)).collect());
        let after = snapshot(base, millis, values.to_vec());
        let deltas = diff_snapshots(&before, &after).expect("diff");
        let mut delta_vec = deltas.deltas().to_vec();
        if multiplexed {
            let before = snapshot(base, 0, values.iter().map(|(role, _)| (*role, 0)).collect());
            let after = snapshot_with_running(base, millis, values.to_vec(), Duration::ZERO);
            delta_vec = diff_snapshots(&before, &after)
                .expect("diff")
                .deltas()
                .to_vec();
        }
        let role_totals = values
            .iter()
            .map(|(role, value)| RoleCounterTotal::new(*role, *value))
            .collect();
        CounterWindow::new(before, after, duration, delta_vec, role_totals)
    }

    fn snapshot(base: Instant, millis: u64, values: Vec<(EventRole, u64)>) -> CounterSnapshot {
        snapshot_with_running(base, millis, values, Duration::from_millis(millis))
    }

    fn snapshot_with_running(
        base: Instant,
        millis: u64,
        values: Vec<(EventRole, u64)>,
        running: Duration,
    ) -> CounterSnapshot {
        let readings = values
            .into_iter()
            .enumerate()
            .map(|(id, (role, value))| {
                CounterReading::new(
                    CounterId::new(id as u32),
                    role,
                    value,
                    Duration::from_millis(millis),
                    running,
                )
                .expect("reading")
            })
            .collect();
        CounterSnapshot::new(
            EvidenceRunId::new(1),
            base + Duration::from_millis(millis),
            readings,
        )
        .expect("snapshot")
    }

    fn input<'a>(
        direct: &'a BenchCase,
        staged: &'a BenchCase,
        source: &'a DeviceInfo,
        destination: &'a DeviceInfo,
        phases: &'a [CounterPhaseReport],
    ) -> VerdictInput<'a> {
        VerdictInput {
            cpu_supported: true,
            source,
            destination,
            direct,
            staged,
            phases,
            acs_redirect: false,
            acs_state_changed: false,
        }
    }

    #[test]
    fn counter_peer_verdict() {
        let direct = bench_case(TransferClass::D2DDirect, 90.0);
        let staged = bench_case(TransferClass::D2DStaged, 30.0);
        let source = device(0, 5, 16, 63.0);
        let destination = device(1, 5, 16, 63.0);
        let phases = vec![
            phase(
                CounterPhase::ExplicitStagedMemory,
                4096,
                &[
                    (EventRole::IioDataReqOfCpuMemReadAllParts, 1024),
                    (EventRole::IioDataReqOfCpuMemWriteAllParts, 1024),
                ],
            ),
            phase(
                CounterPhase::DirectMemory,
                4096,
                &[
                    (EventRole::IioDataReqOfCpuMemReadAllParts, 0),
                    (EventRole::IioDataReqOfCpuMemWriteAllParts, 0),
                ],
            ),
            phase(
                CounterPhase::DirectPeerWrite,
                4096,
                &[(EventRole::IioDataReqOfCpuPeerWriteAllParts, 1024)],
            ),
        ];

        let verdict = synthesize(&input(&direct, &staged, &source, &destination, &phases)).verdict;

        assert!(matches!(
            verdict,
            DiagnosticVerdict::CounterConsistentPeer { .. }
        ));
    }

    #[test]
    fn peer_signal_does_not_confirm_peer_without_valid_low_direct_memory() {
        let direct = bench_case(TransferClass::D2DDirect, 50.0);
        let staged = bench_case(TransferClass::D2DStaged, 30.0);
        let source = device(0, 5, 16, 100.0);
        let destination = device(1, 5, 16, 100.0);
        let staged_phase = phase(
            CounterPhase::ExplicitStagedMemory,
            4096,
            &[
                (EventRole::IioDataReqOfCpuMemReadAllParts, 1024),
                (EventRole::IioDataReqOfCpuMemWriteAllParts, 1024),
            ],
        );
        let peer_phase = phase(
            CounterPhase::DirectPeerWrite,
            4096,
            &[(EventRole::IioDataReqOfCpuPeerWriteAllParts, 1024)],
        );
        let unavailable = CounterPhaseReport::unavailable(
            CounterPhase::DirectMemory,
            4096,
            vec![
                EventRole::IioDataReqOfCpuMemReadAllParts,
                EventRole::IioDataReqOfCpuMemWriteAllParts,
            ],
            Vec::new(),
            Vec::new(),
            EvidenceFailure::other("synthetic direct memory failure"),
        );
        let multiplexed = multiplexed_phase(
            CounterPhase::DirectMemory,
            4096,
            &[
                (EventRole::IioDataReqOfCpuMemReadAllParts, 0),
                (EventRole::IioDataReqOfCpuMemWriteAllParts, 0),
            ],
        );
        let above_negative_gate = phase(
            CounterPhase::DirectMemory,
            4096,
            &[
                (EventRole::IioDataReqOfCpuMemReadAllParts, 100),
                (EventRole::IioDataReqOfCpuMemWriteAllParts, 100),
            ],
        );

        for direct_memory in [unavailable, multiplexed, above_negative_gate] {
            let phases = vec![staged_phase.clone(), direct_memory, peer_phase.clone()];
            let analysis = synthesize(&input(&direct, &staged, &source, &destination, &phases));
            assert!(
                !matches!(
                    analysis.verdict,
                    DiagnosticVerdict::CounterConsistentPeer { .. }
                ),
                "invalid or ambiguous direct-memory evidence must not confirm peer traffic"
            );
        }
    }

    #[test]
    fn counter_host_bounce_verdict() {
        let direct = bench_case(TransferClass::D2DDirect, 30.0);
        let staged = bench_case(TransferClass::D2DStaged, 30.0);
        let source = device(0, 5, 16, 63.0);
        let destination = device(1, 5, 16, 63.0);
        let phases = vec![
            phase(
                CounterPhase::ExplicitStagedMemory,
                4096,
                &[
                    (EventRole::IioDataReqOfCpuMemReadAllParts, 1024),
                    (EventRole::IioDataReqOfCpuMemWriteAllParts, 1024),
                ],
            ),
            phase(
                CounterPhase::DirectMemory,
                4096,
                &[
                    (EventRole::IioDataReqOfCpuMemReadAllParts, 1024),
                    (EventRole::IioDataReqOfCpuMemWriteAllParts, 1024),
                ],
            ),
        ];

        let verdict = synthesize(&input(&direct, &staged, &source, &destination, &phases)).verdict;

        assert_eq!(verdict, DiagnosticVerdict::CounterConsistentHostBounce);
    }

    #[test]
    fn mixed_counter_verdict() {
        let direct = bench_case(TransferClass::D2DDirect, 45.0);
        let staged = bench_case(TransferClass::D2DStaged, 30.0);
        let source = device(0, 5, 16, 63.0);
        let destination = device(1, 5, 16, 63.0);
        let phases = vec![
            phase(
                CounterPhase::ExplicitStagedMemory,
                4096,
                &[
                    (EventRole::IioDataReqOfCpuMemReadAllParts, 1024),
                    (EventRole::IioDataReqOfCpuMemWriteAllParts, 1024),
                ],
            ),
            phase(
                CounterPhase::DirectMemory,
                4096,
                &[
                    (EventRole::IioDataReqOfCpuMemReadAllParts, 1024),
                    (EventRole::IioDataReqOfCpuMemWriteAllParts, 1024),
                ],
            ),
            phase(
                CounterPhase::DirectPeerRead,
                4096,
                &[(EventRole::IioDataReqByCpuPeerReadAllParts, 1024)],
            ),
        ];

        let verdict = synthesize(&input(&direct, &staged, &source, &destination, &phases)).verdict;

        assert!(matches!(
            verdict,
            DiagnosticVerdict::MixedSignalsAcrossRuns { .. }
        ));
    }

    #[test]
    fn staged_calibration_failure_degrades_to_heuristic() {
        let direct = bench_case(TransferClass::D2DDirect, 30.0);
        let staged = bench_case(TransferClass::D2DStaged, 30.0);
        let source = device(0, 5, 16, 63.0);
        let destination = device(1, 5, 16, 63.0);
        let phases = vec![phase(
            CounterPhase::ExplicitStagedMemory,
            4096,
            &[
                (EventRole::IioDataReqOfCpuMemReadAllParts, 10),
                (EventRole::IioDataReqOfCpuMemWriteAllParts, 10),
            ],
        )];

        let verdict = synthesize(&input(&direct, &staged, &source, &destination, &phases)).verdict;

        assert_eq!(
            verdict,
            DiagnosticVerdict::HeuristicOnly {
                likely: LikelyMechanism::HostStaged
            }
        );
    }

    #[test]
    fn baseline_noise_blocks_counter_signal() {
        let direct = bench_case(TransferClass::D2DDirect, 30.0);
        let staged = bench_case(TransferClass::D2DStaged, 30.0);
        let source = device(0, 5, 16, 63.0);
        let destination = device(1, 5, 16, 63.0);
        let roles = [
            EventRole::IioDataReqOfCpuMemReadAllParts,
            EventRole::IioDataReqOfCpuMemWriteAllParts,
        ];
        let noisy_baseline = (0..5)
            .map(|_| synthetic_window(&roles, 2000, false))
            .collect::<Vec<_>>();
        let samples = (0..5)
            .map(|index| {
                CounterSample::new(
                    index,
                    synthetic_window_with_values(&[(roles[0], 1024), (roles[1], 1024)], false),
                )
            })
            .collect::<Vec<_>>();
        let phases = vec![CounterPhaseReport::available(
            CounterPhase::ExplicitStagedMemory,
            4096,
            roles.to_vec(),
            Vec::new(),
            Vec::new(),
            CounterRunReport::new(Duration::from_millis(20), noisy_baseline, samples, None),
        )];

        let analysis = synthesize(&input(&direct, &staged, &source, &destination, &phases));

        assert!(
            analysis
                .reasons
                .iter()
                .any(|reason| reason.code() == EvidenceReasonCode::CounterSignalBelowGate)
        );
    }

    #[test]
    fn multiplexing_and_insufficient_repeats_block_strong_verdicts() {
        let direct = bench_case(TransferClass::D2DDirect, 30.0);
        let staged = bench_case(TransferClass::D2DStaged, 30.0);
        let source = device(0, 5, 16, 63.0);
        let destination = device(1, 5, 16, 63.0);
        let multiplexed = vec![multiplexed_phase(
            CounterPhase::ExplicitStagedMemory,
            4096,
            &[
                (EventRole::IioDataReqOfCpuMemReadAllParts, 1024),
                (EventRole::IioDataReqOfCpuMemWriteAllParts, 1024),
            ],
        )];
        let insufficient = vec![insufficient_phase(
            CounterPhase::ExplicitStagedMemory,
            4096,
            &[
                (EventRole::IioDataReqOfCpuMemReadAllParts, 1024),
                (EventRole::IioDataReqOfCpuMemWriteAllParts, 1024),
            ],
        )];

        let multiplexed_analysis = synthesize(&input(
            &direct,
            &staged,
            &source,
            &destination,
            &multiplexed,
        ));
        let insufficient_analysis = synthesize(&input(
            &direct,
            &staged,
            &source,
            &destination,
            &insufficient,
        ));

        assert!(
            multiplexed_analysis
                .reasons
                .iter()
                .any(|reason| reason.code() == EvidenceReasonCode::CounterMultiplexed)
        );
        assert!(
            insufficient_analysis
                .reasons
                .iter()
                .any(|reason| reason.code() == EvidenceReasonCode::InsufficientCounterRepeats)
        );
    }

    #[test]
    fn terminal_failure_disqualifies_earlier_valid_samples() {
        let direct = bench_case(TransferClass::D2DDirect, 90.0);
        let staged = bench_case(TransferClass::D2DStaged, 30.0);
        let source = device(0, 5, 16, 100.0);
        let destination = device(1, 5, 16, 100.0);
        let phases = vec![
            phase(
                CounterPhase::ExplicitStagedMemory,
                4096,
                &[
                    (EventRole::IioDataReqOfCpuMemReadAllParts, 1024),
                    (EventRole::IioDataReqOfCpuMemWriteAllParts, 1024),
                ],
            ),
            phase_with_terminal_failure(
                CounterPhase::DirectPeerWrite,
                4096,
                &[(EventRole::IioDataReqOfCpuPeerWriteAllParts, 1024)],
            ),
        ];

        let analysis = synthesize(&input(&direct, &staged, &source, &destination, &phases));

        assert!(!matches!(
            analysis.verdict,
            DiagnosticVerdict::CounterConsistentPeer { .. }
                | DiagnosticVerdict::MixedSignalsAcrossRuns { .. }
        ));
        assert!(analysis.reasons.iter().any(|reason| {
            reason.code() == EvidenceReasonCode::CounterTerminalFailure
                && reason.message().contains("terminal read failure")
        }));
    }

    #[test]
    fn upi_terminal_failure_does_not_erase_iio_verdict() {
        let direct = bench_case(TransferClass::D2DDirect, 30.0);
        let staged = bench_case(TransferClass::D2DStaged, 30.0);
        let source = device(0, 5, 16, 100.0);
        let destination = device(1, 5, 16, 100.0);
        let phases = vec![
            phase(
                CounterPhase::ExplicitStagedMemory,
                4096,
                &[
                    (EventRole::IioDataReqOfCpuMemReadAllParts, 1024),
                    (EventRole::IioDataReqOfCpuMemWriteAllParts, 1024),
                ],
            ),
            phase(
                CounterPhase::DirectMemory,
                4096,
                &[
                    (EventRole::IioDataReqOfCpuMemReadAllParts, 1024),
                    (EventRole::IioDataReqOfCpuMemWriteAllParts, 1024),
                ],
            ),
            phase_with_terminal_failure(
                CounterPhase::DirectUpi,
                4096,
                &[(EventRole::UpiTxDataFlitsAll, 1024)],
            ),
        ];

        let analysis = synthesize(&input(&direct, &staged, &source, &destination, &phases));

        assert_eq!(
            analysis.verdict,
            DiagnosticVerdict::CounterConsistentHostBounce
        );
    }

    #[test]
    fn baseline_is_scaled_to_longer_copy_window_with_integer_ceil() {
        let roles = [
            EventRole::IioDataReqOfCpuMemReadAllParts,
            EventRole::IioDataReqOfCpuMemWriteAllParts,
        ];
        let baseline = (0..5)
            .map(|_| {
                synthetic_window_with_duration(
                    &[(roles[0], 100), (roles[1], 100)],
                    false,
                    Duration::from_millis(20),
                )
            })
            .collect::<Vec<_>>();
        let samples = (0..5)
            .map(|index| {
                CounterSample::new(
                    index,
                    synthetic_window_with_duration(
                        &[(roles[0], 540), (roles[1], 540)],
                        false,
                        Duration::from_millis(100),
                    ),
                )
            })
            .collect::<Vec<_>>();
        let phase = CounterPhaseReport::available(
            CounterPhase::ExplicitStagedMemory,
            4096,
            roles.to_vec(),
            Vec::new(),
            Vec::new(),
            CounterRunReport::new(Duration::from_millis(20), baseline, samples, None),
        );
        let mut reasons = Vec::new();

        let gate = gated_role(Some(&phase), roles[0], &mut reasons);

        assert!(!gate.passes);
        assert!(
            reasons
                .iter()
                .any(|reason| reason.code() == EvidenceReasonCode::CounterSignalBelowGate)
        );
    }

    #[test]
    fn heuristic_host_device_and_link() {
        let source_fast = device(0, 5, 16, 100.0);
        let destination_fast = device(1, 5, 16, 100.0);
        let source_low = device(0, 4, 8, 16.0);
        let destination_low = device(1, 4, 8, 16.0);

        let host = synthesize(&input(
            &bench_case(TransferClass::D2DDirect, 30.0),
            &bench_case(TransferClass::D2DStaged, 30.0),
            &source_fast,
            &destination_fast,
            &[],
        ))
        .verdict;
        let device_side = synthesize(&input(
            &bench_case(TransferClass::D2DDirect, 70.0),
            &bench_case(TransferClass::D2DStaged, 30.0),
            &source_fast,
            &destination_fast,
            &[],
        ))
        .verdict;
        let link_limited = synthesize(&input(
            &bench_case(TransferClass::D2DDirect, 12.0),
            &bench_case(TransferClass::D2DStaged, 4.0),
            &source_low,
            &destination_low,
            &[],
        ))
        .verdict;

        assert_eq!(
            host,
            DiagnosticVerdict::HeuristicOnly {
                likely: LikelyMechanism::HostStaged
            }
        );
        assert_eq!(
            device_side,
            DiagnosticVerdict::HeuristicOnly {
                likely: LikelyMechanism::DeviceSide
            }
        );
        assert_eq!(
            link_limited,
            DiagnosticVerdict::HeuristicOnly {
                likely: LikelyMechanism::LinkLimited
            }
        );
    }

    #[test]
    fn separated_clusters_disable_median_based_heuristics() {
        let mut direct = bench_case(TransferClass::D2DDirect, 40.0);
        let CaseOutcome::Measured { summary, .. } = &mut direct.outcome else {
            panic!("test case must be measured");
        };
        summary.shape = DistributionShape::SeparatedClusters(SeparatedClusters {
            lower_center: 30.0,
            lower_count: 8,
            upper_center: 42.0,
            upper_count: 12,
        });
        let staged = bench_case(TransferClass::D2DStaged, 30.0);
        let source = device(0, 5, 16, 63.0);
        let destination = device(1, 5, 16, 63.0);

        let analysis = synthesize(&input(&direct, &staged, &source, &destination, &[]));

        assert_eq!(analysis.verdict, DiagnosticVerdict::Indeterminate);
        assert!(
            analysis
                .reasons
                .iter()
                .any(|reason| { reason.code() == EvidenceReasonCode::ThroughputSeparatedClusters })
        );
        assert!(analysis.reasons.iter().any(|reason| {
            reason.code() == EvidenceReasonCode::HeuristicUnavailable
                && reason.message().contains("median-based")
        }));
    }

    #[test]
    fn acs_redirect_qualifies_peer_not_host_bounce() {
        let direct = bench_case(TransferClass::D2DDirect, 90.0);
        let staged = bench_case(TransferClass::D2DStaged, 30.0);
        let source = device(0, 5, 16, 63.0);
        let destination = device(1, 5, 16, 63.0);
        let phases = vec![
            phase(
                CounterPhase::ExplicitStagedMemory,
                4096,
                &[
                    (EventRole::IioDataReqOfCpuMemReadAllParts, 1024),
                    (EventRole::IioDataReqOfCpuMemWriteAllParts, 1024),
                ],
            ),
            phase(
                CounterPhase::DirectMemory,
                4096,
                &[
                    (EventRole::IioDataReqOfCpuMemReadAllParts, 0),
                    (EventRole::IioDataReqOfCpuMemWriteAllParts, 0),
                ],
            ),
            phase(
                CounterPhase::DirectPeerWrite,
                4096,
                &[(EventRole::IioDataReqOfCpuPeerWriteAllParts, 1024)],
            ),
        ];
        let mut input = input(&direct, &staged, &source, &destination, &phases);
        input.acs_redirect = true;

        let verdict = synthesize(&input).verdict;

        assert_eq!(
            verdict,
            DiagnosticVerdict::CounterConsistentPeer {
                route_qualifier: Some(
                    PeerRouteQualifier::AcsRedirectedOrUpstreamRoutedPeerTrafficNotHostBounce
                )
            }
        );
    }

    #[test]
    fn unsupported_cpu_degrades_to_heuristic_and_spr_missing_of_read_still_uses_by_read() {
        let direct = bench_case(TransferClass::D2DDirect, 30.0);
        let staged = bench_case(TransferClass::D2DStaged, 30.0);
        let source = device(0, 5, 16, 63.0);
        let destination = device(1, 5, 16, 63.0);
        let unsupported = VerdictInput {
            cpu_supported: false,
            source: &source,
            destination: &destination,
            direct: &direct,
            staged: &staged,
            phases: &[],
            acs_redirect: false,
            acs_state_changed: false,
        };
        assert!(matches!(
            synthesize(&unsupported).verdict,
            DiagnosticVerdict::HeuristicOnly { .. }
        ));

        let phases = vec![
            phase(
                CounterPhase::ExplicitStagedMemory,
                4096,
                &[
                    (EventRole::IioDataReqOfCpuMemReadAllParts, 1024),
                    (EventRole::IioDataReqOfCpuMemWriteAllParts, 1024),
                ],
            ),
            phase(
                CounterPhase::DirectMemory,
                4096,
                &[
                    (EventRole::IioDataReqOfCpuMemReadAllParts, 0),
                    (EventRole::IioDataReqOfCpuMemWriteAllParts, 0),
                ],
            ),
            phase(
                CounterPhase::DirectPeerRead,
                4096,
                &[(EventRole::IioDataReqByCpuPeerReadAllParts, 1024)],
            ),
        ];
        assert!(matches!(
            synthesize(&input(&direct, &staged, &source, &destination, &phases)).verdict,
            DiagnosticVerdict::CounterConsistentPeer { .. }
        ));
    }

    #[test]
    fn overflow_failure_is_recorded_as_counter_unavailable() {
        let direct = bench_case(TransferClass::D2DDirect, 30.0);
        let staged = bench_case(TransferClass::D2DStaged, 30.0);
        let source = device(0, 5, 16, 63.0);
        let destination = device(1, 5, 16, 63.0);
        let phases = vec![CounterPhaseReport::unavailable(
            CounterPhase::ExplicitStagedMemory,
            4096,
            vec![],
            vec![],
            vec![],
            EvidenceFailure::counter_overflow("counter aggregate overflow"),
        )];

        let analysis = synthesize(&input(&direct, &staged, &source, &destination, &phases));

        assert!(
            analysis
                .reasons
                .iter()
                .any(|reason| reason.code() == EvidenceReasonCode::CounterOverflow)
        );
    }
}
