#![allow(clippy::module_name_repetitions)]

mod counter;
mod model;
mod progress;
mod verdict;

use crate::benchmark;
use crate::benchmark::measurement::NoopMeasurementObserver;
use crate::cli::{BenchMode, BenchOptions, MIN_SAMPLES, OutputFormat, TimingMode, TransferClass};
use crate::evidence::acs::BridgeOutcome;
use crate::evidence::counters::EvidenceRunId;
use crate::evidence::cpu::read_cpu_identity;
use crate::evidence::intel_perfmon::ATTRIBUTION;
use crate::evidence::{CpuProfile, PmuInstance, PmuKind, discover_pmus, read_acs_bridge_path};
use crate::output::{
    BenchCase, BenchReport, CaseOutcome, DeviceInfo, Endpoint, Operation, PeerAccess, PeerRoute,
};
use crate::pcie::PciAddress;

use self::counter::{
    CounterPhaseObserver, MissingRolePolicy, collect_idle_baselines, peer_read_roles,
    peer_write_roles, prepare_phase, required_memory_roles, upi_roles,
};
pub use self::model::{
    CounterEventReport, CounterGroupReport, CounterPhase, CounterPhaseReport, CounterRunReport,
    CounterSample, CounterWindow, CpuEvidence, DiagnosticError, DiagnosticVerdict,
    EvidenceAvailability, EvidenceFailure, EvidenceFailureKind, EvidenceReason, EvidenceReasonCode,
    LikelyMechanism, MissingCounterRole, P2pDiagnosticOptions, P2pDiagnosticReport,
    PeerRouteQualifier, RoleCounterTotal,
};
use self::model::{availability_from_failure, link_theoretical_gb_s};
pub use self::progress::{
    DiagnosticPhase, DiagnosticPhaseStatus, NoopP2pDiagnosticProgress, P2P_DIAGNOSTIC_PHASES,
    P2pDiagnosticProgress, phase_position,
};
use self::verdict::{VerdictInput, synthesize};

#[allow(clippy::too_many_lines)]
pub fn diag_p2p(options: &P2pDiagnosticOptions) -> Result<P2pDiagnosticReport, DiagnosticError> {
    diag_p2p_with_reporter(options, NoopP2pDiagnosticProgress::default())
}

#[allow(clippy::too_many_lines)]
pub fn diag_p2p_with_reporter<R>(
    options: &P2pDiagnosticOptions,
    mut reporter: R,
) -> Result<P2pDiagnosticReport, DiagnosticError>
where
    R: P2pDiagnosticProgress,
{
    validate_options(options)?;

    phase_started(&mut reporter, DiagnosticPhase::Discovery)?;
    let cpu = discover_cpu();
    let iio_pmus = discover_runtime_pmus(cpu.profile_value(), PmuKind::Iio);
    let upi_pmus = discover_runtime_pmus(cpu.profile_value(), PmuKind::UpiLl);
    let discovered = benchmark::list()?;
    let initial_source = selected_device(&discovered.devices, options.source_device())?;
    let initial_destination = selected_device(&discovered.devices, options.destination_device())?;
    let initial_acs = collect_acs(&initial_source, &initial_destination);
    let mut next_counter_id = 0;
    let mut next_run_id = 1;
    phase_finished(
        &mut reporter,
        DiagnosticPhase::Discovery,
        DiagnosticPhaseStatus::Complete,
        None,
    )?;

    let staged_preparation = prepare_phase(
        CounterPhase::ExplicitStagedMemory,
        options.size_bytes(),
        cpu.profile_value(),
        &iio_pmus,
        PmuKind::Iio,
        &required_memory_roles(),
        MissingRolePolicy::Fail,
        EvidenceRunId::new(next_run(&mut next_run_id)),
        &mut next_counter_id,
    );
    let (staged, staged_devices, mut staged_phase) = run_required_phase(
        options,
        TransferClass::D2DStaged,
        staged_preparation,
        &mut reporter,
    )?;

    let direct_preparation = prepare_phase(
        CounterPhase::DirectMemory,
        options.size_bytes(),
        cpu.profile_value(),
        &iio_pmus,
        PmuKind::Iio,
        &required_memory_roles(),
        MissingRolePolicy::Fail,
        EvidenceRunId::new(next_run(&mut next_run_id)),
        &mut next_counter_id,
    );
    let (direct, direct_devices, mut direct_phase) = run_required_phase(
        options,
        TransferClass::D2DDirect,
        direct_preparation,
        &mut reporter,
    )?;

    validate_requested_endpoints(&direct, options)?;
    let source = selected_device(&direct_devices, options.source_device())?;
    let destination = selected_device(&direct_devices, options.destination_device())?;
    let canonical = CanonicalPair {
        source: source.clone(),
        destination: destination.clone(),
    };
    let canonical_failure = validate_topology_identity(&direct_devices, &canonical).err();
    if let Some(failure) = &canonical_failure {
        direct_phase = direct_phase.invalidate(failure.clone());
        staged_phase = staged_phase.invalidate(EvidenceFailure::topology_mismatch(format!(
            "staged evidence cannot be matched because canonical direct identity is unavailable: {}",
            failure.message()
        )));
    } else if let Err(failure) =
        validate_repeated_pair_identity(&staged, &staged_devices, &canonical)
    {
        staged_phase = staged_phase.invalidate(failure);
    }
    let (peer_access, route) = direct_peer_info(&direct);

    let mut phases = vec![staged_phase, direct_phase];
    phases.push(run_optional_direct_phase(
        options,
        prepare_phase(
            CounterPhase::DirectPeerWrite,
            options.size_bytes(),
            cpu.profile_value(),
            &iio_pmus,
            PmuKind::Iio,
            &peer_write_roles(),
            MissingRolePolicy::Skip,
            EvidenceRunId::new(next_run(&mut next_run_id)),
            &mut next_counter_id,
        ),
        &canonical,
        canonical_failure.as_ref(),
        &mut reporter,
    )?);
    phases.push(run_optional_direct_phase(
        options,
        prepare_phase(
            CounterPhase::DirectPeerRead,
            options.size_bytes(),
            cpu.profile_value(),
            &iio_pmus,
            PmuKind::Iio,
            &peer_read_roles(),
            MissingRolePolicy::Skip,
            EvidenceRunId::new(next_run(&mut next_run_id)),
            &mut next_counter_id,
        ),
        &canonical,
        canonical_failure.as_ref(),
        &mut reporter,
    )?);
    phases.push(run_optional_direct_phase(
        options,
        prepare_phase(
            CounterPhase::DirectUpi,
            options.size_bytes(),
            cpu.profile_value(),
            &upi_pmus,
            PmuKind::UpiLl,
            &upi_roles(),
            MissingRolePolicy::Fail,
            EvidenceRunId::new(next_run(&mut next_run_id)),
            &mut next_counter_id,
        ),
        &canonical,
        canonical_failure.as_ref(),
        &mut reporter,
    )?);

    phase_started(&mut reporter, DiagnosticPhase::Acs)?;
    let acs = collect_acs(&source, &destination);
    let acs_state_changed = initial_acs != acs;
    let acs_redirect = !acs_state_changed && acs_redirect_observed(&acs);
    let (acs_status, acs_reason) = match &acs {
        EvidenceAvailability::Available(_) => (DiagnosticPhaseStatus::Complete, None),
        EvidenceAvailability::PermissionDenied(failure)
        | EvidenceAvailability::Unsupported(failure)
        | EvidenceAvailability::Malformed(failure)
        | EvidenceAvailability::ResourceUnavailable(failure)
        | EvidenceAvailability::Io(failure)
        | EvidenceAvailability::Other(failure) => {
            (DiagnosticPhaseStatus::Unavailable, Some(failure))
        }
    };
    phase_finished(&mut reporter, DiagnosticPhase::Acs, acs_status, acs_reason)?;

    phase_started(&mut reporter, DiagnosticPhase::Synthesis)?;
    let analysis = synthesize(&VerdictInput {
        cpu_supported: cpu.profile_value().is_some(),
        source: &source,
        destination: &destination,
        direct: &direct,
        staged: &staged,
        phases: &phases,
        acs_redirect,
        acs_state_changed,
    });
    phase_finished(
        &mut reporter,
        DiagnosticPhase::Synthesis,
        DiagnosticPhaseStatus::Complete,
        None,
    )?;

    Ok(P2pDiagnosticReport::new(
        source,
        destination,
        peer_access,
        route,
        direct,
        staged,
        cpu,
        phases,
        acs,
        analysis.verdict,
        analysis.reasons,
        ATTRIBUTION,
    ))
}

fn phase_started<R>(reporter: &mut R, phase: DiagnosticPhase) -> Result<(), DiagnosticError>
where
    R: P2pDiagnosticProgress,
{
    reporter
        .phase_started(phase)
        .map_err(DiagnosticError::Reporter)
}

fn phase_finished<R>(
    reporter: &mut R,
    phase: DiagnosticPhase,
    status: DiagnosticPhaseStatus,
    reason: Option<&EvidenceFailure>,
) -> Result<(), DiagnosticError>
where
    R: P2pDiagnosticProgress,
{
    reporter
        .phase_finished(phase, status, reason)
        .map_err(DiagnosticError::Reporter)
}

fn validate_options(options: &P2pDiagnosticOptions) -> Result<(), DiagnosticError> {
    if options.source_device() == options.destination_device() {
        return Err(DiagnosticError::InvalidOptions(
            "P2P diagnostics require distinct source and destination devices".to_owned(),
        ));
    }
    if options.size_bytes() == 0 {
        return Err(DiagnosticError::InvalidOptions(
            "P2P diagnostics require a non-zero transfer size".to_owned(),
        ));
    }
    usize::try_from(options.size_bytes()).map_err(|_| {
        DiagnosticError::InvalidOptions(format!(
            "transfer size {} does not fit in this process",
            options.size_bytes()
        ))
    })?;
    if options.samples() < MIN_SAMPLES {
        return Err(DiagnosticError::InvalidOptions(format!(
            "P2P diagnostics require at least {MIN_SAMPLES} samples"
        )));
    }
    usize::try_from(options.samples()).map_err(|_| {
        DiagnosticError::InvalidOptions(format!(
            "sample count {} does not fit in this process",
            options.samples()
        ))
    })?;
    Ok(())
}

fn discover_cpu() -> CpuEvidence {
    match read_cpu_identity() {
        Ok(identity) => {
            let profile = match identity.matching_profile() {
                Ok(profile) => EvidenceAvailability::Available(profile),
                Err(error) => availability_from_failure(error.into()),
            };
            CpuEvidence::new(EvidenceAvailability::Available(identity), profile)
        }
        Err(error) => {
            let failure: EvidenceFailure = error.into();
            CpuEvidence::new(
                availability_from_failure(failure.clone()),
                availability_from_failure(failure),
            )
        }
    }
}

fn discover_runtime_pmus(
    profile: Option<CpuProfile>,
    kind: PmuKind,
) -> EvidenceAvailability<Vec<PmuInstance>> {
    if profile.is_none() {
        return EvidenceAvailability::Unsupported(EvidenceFailure::unsupported(format!(
            "{kind} PMUs were not discovered because the CPU profile is unsupported"
        )));
    }
    match discover_pmus(kind) {
        Ok(pmus) => EvidenceAvailability::Available(pmus),
        Err(error) => availability_from_failure(error.into()),
    }
}

fn next_run(next: &mut u64) -> u64 {
    let value = *next;
    *next = next.saturating_add(1);
    value
}

fn run_required_phase(
    options: &P2pDiagnosticOptions,
    class: TransferClass,
    mut preparation: counter::PhaseCounterPreparation,
    reporter: &mut impl P2pDiagnosticProgress,
) -> Result<(BenchCase, Vec<DeviceInfo>, CounterPhaseReport), DiagnosticError> {
    let diagnostic_phase = DiagnosticPhase::from_counter_phase(preparation.phase);
    phase_started(reporter, diagnostic_phase)?;
    let Some(mut counter_set) = preparation.counter_set.take() else {
        let (case, devices) = run_noop_case(options, class, preparation.phase, reporter)?;
        let phase = preparation.unavailable_report();
        phase_finished(
            reporter,
            diagnostic_phase,
            DiagnosticPhaseStatus::Unavailable,
            phase.evidence().failure(),
        )?;
        return Ok((case, devices, phase));
    };

    let baselines = match collect_idle_baselines(&mut counter_set) {
        Ok(baselines) => baselines,
        Err(failure) => {
            preparation.failure = Some(failure);
            let (case, devices) = run_noop_case(options, class, preparation.phase, reporter)?;
            let phase = preparation.unavailable_report();
            phase_finished(
                reporter,
                diagnostic_phase,
                DiagnosticPhaseStatus::Unavailable,
                phase.evidence().failure(),
            )?;
            return Ok((case, devices, phase));
        }
    };

    let mut observer = CounterPhaseObserver::new(counter_set);
    let report = benchmark::bench_with_reporter_and_observer(
        &bench_options(options, class),
        reporter,
        &mut observer,
    )?;
    let (case, devices) = extract_case(report, preparation.phase, class)?;
    if let CaseOutcome::Skipped { reason } = &case.outcome {
        preparation.failure = Some(EvidenceFailure::resource_unavailable(format!(
            "{} required benchmark was skipped: {reason}",
            preparation.phase
        )));
        let phase = preparation.unavailable_report();
        phase_finished(
            reporter,
            diagnostic_phase,
            DiagnosticPhaseStatus::Unavailable,
            phase.evidence().failure(),
        )?;
        return Ok((case, devices, phase));
    }
    let (samples, terminal_failure) = observer.into_parts();
    let phase = preparation.available_report(baselines, samples, terminal_failure);
    let (status, failure) = phase_completion(&phase);
    phase_finished(reporter, diagnostic_phase, status, failure)?;
    Ok((case, devices, phase))
}

fn run_optional_direct_phase(
    options: &P2pDiagnosticOptions,
    mut preparation: counter::PhaseCounterPreparation,
    canonical: &CanonicalPair,
    canonical_failure: Option<&EvidenceFailure>,
    reporter: &mut impl P2pDiagnosticProgress,
) -> Result<CounterPhaseReport, DiagnosticError> {
    let diagnostic_phase = DiagnosticPhase::from_counter_phase(preparation.phase);
    phase_started(reporter, diagnostic_phase)?;
    if let Some(failure) = canonical_failure {
        preparation.failure = Some(EvidenceFailure::topology_mismatch(format!(
            "{} cannot run because canonical direct identity is unavailable: {}",
            preparation.phase,
            failure.message()
        )));
        let phase = preparation.unavailable_report();
        phase_finished(
            reporter,
            diagnostic_phase,
            DiagnosticPhaseStatus::Unavailable,
            phase.evidence().failure(),
        )?;
        return Ok(phase);
    }
    let Some(mut counter_set) = preparation.counter_set.take() else {
        let phase = preparation.unavailable_report();
        phase_finished(
            reporter,
            diagnostic_phase,
            DiagnosticPhaseStatus::Unavailable,
            phase.evidence().failure(),
        )?;
        return Ok(phase);
    };
    let baselines = match collect_idle_baselines(&mut counter_set) {
        Ok(baselines) => baselines,
        Err(failure) => {
            preparation.failure = Some(failure);
            let phase = preparation.unavailable_report();
            phase_finished(
                reporter,
                diagnostic_phase,
                DiagnosticPhaseStatus::Unavailable,
                phase.evidence().failure(),
            )?;
            return Ok(phase);
        }
    };

    let mut observer = CounterPhaseObserver::new(counter_set);
    let report = match benchmark::bench_with_reporter_and_observer(
        &bench_options(options, TransferClass::D2DDirect),
        reporter,
        &mut observer,
    ) {
        Ok(report) => report,
        Err(error) => {
            preparation.failure = Some(EvidenceFailure::other(format!(
                "{} optional direct benchmark failed: {error}",
                preparation.phase
            )));
            let phase = preparation.unavailable_report();
            phase_finished(
                reporter,
                diagnostic_phase,
                DiagnosticPhaseStatus::Unavailable,
                phase.evidence().failure(),
            )?;
            return Ok(phase);
        }
    };
    let (case, devices) = extract_case(report, preparation.phase, TransferClass::D2DDirect)?;
    if let Err(failure) = validate_repeated_pair_identity(&case, &devices, canonical) {
        preparation.failure = Some(failure);
        let phase = preparation.unavailable_report();
        phase_finished(
            reporter,
            diagnostic_phase,
            DiagnosticPhaseStatus::Unavailable,
            phase.evidence().failure(),
        )?;
        return Ok(phase);
    }
    if let CaseOutcome::Skipped { reason } = &case.outcome {
        preparation.failure = Some(EvidenceFailure::resource_unavailable(format!(
            "{} optional direct benchmark was skipped: {reason}",
            preparation.phase
        )));
        let phase = preparation.unavailable_report();
        phase_finished(
            reporter,
            diagnostic_phase,
            DiagnosticPhaseStatus::Unavailable,
            phase.evidence().failure(),
        )?;
        return Ok(phase);
    }
    let (samples, terminal_failure) = observer.into_parts();
    let phase = preparation.available_report(baselines, samples, terminal_failure);
    let (status, failure) = phase_completion(&phase);
    phase_finished(reporter, diagnostic_phase, status, failure)?;
    Ok(phase)
}

fn phase_completion(
    phase: &CounterPhaseReport,
) -> (DiagnosticPhaseStatus, Option<&EvidenceFailure>) {
    match phase.evidence() {
        EvidenceAvailability::Available(run) => run
            .terminal_failure()
            .map_or((DiagnosticPhaseStatus::Complete, None), |failure| {
                (DiagnosticPhaseStatus::Unavailable, Some(failure))
            }),
        EvidenceAvailability::PermissionDenied(failure)
        | EvidenceAvailability::Unsupported(failure)
        | EvidenceAvailability::Malformed(failure)
        | EvidenceAvailability::ResourceUnavailable(failure)
        | EvidenceAvailability::Io(failure)
        | EvidenceAvailability::Other(failure) => {
            (DiagnosticPhaseStatus::Unavailable, Some(failure))
        }
    }
}

fn run_noop_case(
    options: &P2pDiagnosticOptions,
    class: TransferClass,
    phase: CounterPhase,
    reporter: &mut impl P2pDiagnosticProgress,
) -> Result<(BenchCase, Vec<DeviceInfo>), DiagnosticError> {
    let mut observer = NoopMeasurementObserver;
    let report = benchmark::bench_with_reporter_and_observer(
        &bench_options(options, class),
        reporter,
        &mut observer,
    )?;
    extract_case(report, phase, class)
}

fn bench_options(options: &P2pDiagnosticOptions, class: TransferClass) -> BenchOptions {
    BenchOptions {
        device: Some(options.source_device()),
        peer_device: Some(options.destination_device()),
        transfer_class: Some(class),
        queue_group: options.queue_group(),
        size_bytes: options.size_bytes(),
        samples: options.samples(),
        warmup: options.warmup(),
        timing: TimingMode::WallClock,
        format: OutputFormat::Text,
        histogram: false,
        summary_only: true,
        mode: BenchMode::Saturation,
    }
}

fn extract_case(
    report: BenchReport,
    phase: CounterPhase,
    class: TransferClass,
) -> Result<(BenchCase, Vec<DeviceInfo>), DiagnosticError> {
    if report.cases.len() != 1 {
        return Err(DiagnosticError::UnexpectedCaseCount {
            phase,
            count: report.cases.len(),
        });
    }
    let devices = report.system.devices;
    let case = report
        .cases
        .into_iter()
        .next()
        .ok_or(DiagnosticError::UnexpectedCaseCount { phase, count: 0 })?;
    if case.transfer_class != class {
        return Err(DiagnosticError::UnexpectedCaseShape {
            phase,
            reason: format!("expected {class}, got {}", case.transfer_class),
        });
    }
    Ok((case, devices))
}

fn validate_requested_endpoints(
    case: &BenchCase,
    options: &P2pDiagnosticOptions,
) -> Result<(), DiagnosticError> {
    let expected_source = Endpoint::Device(options.source_device());
    let expected_destination = Endpoint::Device(options.destination_device());
    if case.source == expected_source && case.destination == expected_destination {
        Ok(())
    } else {
        Err(DiagnosticError::UnexpectedCaseShape {
            phase: CounterPhase::DirectMemory,
            reason: format!(
                "expected {} -> {}, got {} -> {}",
                expected_source, expected_destination, case.source, case.destination
            ),
        })
    }
}

#[derive(Clone, Debug)]
struct CanonicalPair {
    source: DeviceInfo,
    destination: DeviceInfo,
}

fn validate_topology_identity(
    devices: &[DeviceInfo],
    canonical: &CanonicalPair,
) -> Result<(), EvidenceFailure> {
    validate_device_identity(devices, &canonical.source)?;
    validate_device_identity(devices, &canonical.destination)
}

fn validate_repeated_pair_identity(
    case: &BenchCase,
    devices: &[DeviceInfo],
    canonical: &CanonicalPair,
) -> Result<(), EvidenceFailure> {
    if case.source != Endpoint::Device(canonical.source.index)
        || case.destination != Endpoint::Device(canonical.destination.index)
    {
        return Err(EvidenceFailure::topology_mismatch(format!(
            "repeated case endpoints changed from dev{} -> dev{} to {} -> {}",
            canonical.source.index, canonical.destination.index, case.source, case.destination
        )));
    }
    validate_topology_identity(devices, canonical)
}

fn validate_device_identity(
    devices: &[DeviceInfo],
    canonical: &DeviceInfo,
) -> Result<(), EvidenceFailure> {
    let matches = devices
        .iter()
        .filter(|device| device.index == canonical.index)
        .collect::<Vec<_>>();
    let [actual] = matches.as_slice() else {
        return Err(EvidenceFailure::topology_mismatch(format!(
            "expected exactly one dev{} in repeated topology, found {}",
            canonical.index,
            matches.len()
        )));
    };
    let Some(expected_bdf) = canonical.pci_address.as_deref() else {
        return Err(EvidenceFailure::topology_mismatch(format!(
            "canonical direct dev{} has no PCI address",
            canonical.index
        )));
    };
    let Some(expected_address) = PciAddress::parse(expected_bdf) else {
        return Err(EvidenceFailure::topology_mismatch(format!(
            "canonical direct dev{} PCI address '{expected_bdf}' is malformed",
            canonical.index
        )));
    };
    let Some(actual_bdf) = actual.pci_address.as_deref() else {
        return Err(EvidenceFailure::topology_mismatch(format!(
            "repeated topology dev{} has no PCI address; expected {expected_bdf}",
            canonical.index
        )));
    };
    let Some(actual_address) = PciAddress::parse(actual_bdf) else {
        return Err(EvidenceFailure::topology_mismatch(format!(
            "repeated topology dev{} PCI address '{actual_bdf}' is malformed",
            canonical.index
        )));
    };
    if actual_address != expected_address {
        return Err(EvidenceFailure::topology_mismatch(format!(
            "repeated topology dev{} PCI address changed from {expected_bdf} to {actual_bdf}",
            canonical.index
        )));
    }
    Ok(())
}

fn selected_device(devices: &[DeviceInfo], index: u32) -> Result<DeviceInfo, DiagnosticError> {
    devices
        .iter()
        .find(|device| device.index == index)
        .cloned()
        .ok_or(DiagnosticError::MissingDevice { index })
}

fn direct_peer_info(case: &BenchCase) -> (PeerAccess, PeerRoute) {
    match &case.operation {
        Operation::Direct { peer_access, route } => (peer_access.clone(), route.clone()),
        Operation::ExplicitStaged { route } => (
            PeerAccess::Unknown("not a direct case".to_owned()),
            route.clone(),
        ),
        Operation::HostToDevice | Operation::DeviceToHost | Operation::SameDevice => (
            PeerAccess::Unknown("not a cross-device case".to_owned()),
            PeerRoute::Unknown("not a cross-device case".to_owned()),
        ),
    }
}

fn collect_acs(
    source: &DeviceInfo,
    destination: &DeviceInfo,
) -> EvidenceAvailability<crate::evidence::AcsBridgePathEvidence> {
    let source = match parse_device_bdf(source) {
        Ok(address) => address,
        Err(failure) => return availability_from_failure(failure),
    };
    let destination = match parse_device_bdf(destination) {
        Ok(address) => address,
        Err(failure) => return availability_from_failure(failure),
    };
    match read_acs_bridge_path(source, destination) {
        Ok(evidence) => EvidenceAvailability::Available(evidence),
        Err(crate::pcie::PcieLinkUnknown::UnreadableField {
            kind: std::io::ErrorKind::PermissionDenied,
            path,
            error,
        }) => EvidenceAvailability::PermissionDenied(EvidenceFailure::new(
            self::model::EvidenceFailureKind::PermissionDenied,
            format!("cannot read ACS bridge path at {}: {error}", path.display()),
        )),
        Err(error) => EvidenceAvailability::Io(EvidenceFailure::new(
            self::model::EvidenceFailureKind::Io,
            format!("cannot read ACS bridge path: {error:?}"),
        )),
    }
}

fn parse_device_bdf(device: &DeviceInfo) -> Result<PciAddress, EvidenceFailure> {
    let Some(address) = &device.pci_address else {
        return Err(EvidenceFailure::unsupported(format!(
            "dev{} has no Level Zero PCI address",
            device.index
        )));
    };
    PciAddress::parse(address).ok_or_else(|| {
        EvidenceFailure::malformed(format!(
            "dev{} PCI address '{address}' is malformed",
            device.index
        ))
    })
}

fn acs_redirect_observed(
    acs: &EvidenceAvailability<crate::evidence::AcsBridgePathEvidence>,
) -> bool {
    match acs {
        EvidenceAvailability::Available(path) => path
            .bridges()
            .iter()
            .any(|bridge| matches!(bridge.result(), Ok(BridgeOutcome::RedirectObserved(_)))),
        EvidenceAvailability::PermissionDenied(_)
        | EvidenceAvailability::Unsupported(_)
        | EvidenceAvailability::Malformed(_)
        | EvidenceAvailability::ResourceUnavailable(_)
        | EvidenceAvailability::Io(_)
        | EvidenceAvailability::Other(_) => false,
    }
}

#[allow(dead_code)]
fn _theoretical_link_for_future_rendering(device: &DeviceInfo) -> Option<f64> {
    link_theoretical_gb_s(&device.pcie_link)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::evidence::intel_perfmon::EventRole;
    use crate::output::{AllocationKind, HostInfo, LinkInfo, PeerRoute, SystemInfo};
    use crate::stats::{
        ConfidenceInterval, Quartiles, Summary, TukeyFences, TukeyOutlierCounts, TukeyOutliers,
    };

    use super::*;

    #[test]
    fn rejects_same_device_options() {
        let options = P2pDiagnosticOptions::new(1, 1, 4096, MIN_SAMPLES, Duration::ZERO);
        assert!(matches!(
            validate_options(&options),
            Err(DiagnosticError::InvalidOptions(_))
        ));
    }

    #[test]
    fn rejects_zero_size_and_too_few_samples_before_benchmark() {
        let zero_size = P2pDiagnosticOptions::new(0, 1, 0, MIN_SAMPLES, Duration::ZERO);
        let too_few = P2pDiagnosticOptions::new(0, 1, 4096, MIN_SAMPLES - 1, Duration::ZERO);

        assert!(matches!(
            validate_options(&zero_size),
            Err(DiagnosticError::InvalidOptions(message)) if message.contains("non-zero")
        ));
        assert!(matches!(
            validate_options(&too_few),
            Err(DiagnosticError::InvalidOptions(message)) if message.contains("at least")
        ));
    }

    #[test]
    fn terminal_counter_failure_finishes_phase_as_unavailable() {
        let phase = CounterPhaseReport::available(
            CounterPhase::DirectUpi,
            4096,
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

        let (status, failure) = phase_completion(&phase);

        assert_eq!(status, DiagnosticPhaseStatus::Unavailable);
        assert_eq!(
            failure.map(EvidenceFailure::message),
            Some("counter read failed")
        );
    }

    #[test]
    fn parses_endpoint_bdf_structurally() {
        assert_eq!(
            PciAddress::parse("0000:17:02.1"),
            PciAddress::new(0, 0x17, 0x02, 1)
        );
        assert_eq!(PciAddress::parse("0000:17:02"), None);
        assert_eq!(PciAddress::parse("0000:17:20.0"), None);
    }

    #[test]
    fn skipped_staged_case_is_retained_and_synthesizes_indeterminate() {
        let devices = vec![
            device(0, Some("0000:17:00.0")),
            device(1, Some("0000:18:00.0")),
        ];
        let staged_report = report_with_case(
            devices.clone(),
            skipped_case(TransferClass::D2DStaged, "staged unavailable"),
        );
        let (staged, staged_devices) = extract_case(
            staged_report,
            CounterPhase::ExplicitStagedMemory,
            TransferClass::D2DStaged,
        )
        .expect("skipped case remains report data");
        let direct_report = report_with_case(devices, measured_direct_case(1.0));
        let (direct, _) = extract_case(
            direct_report,
            CounterPhase::DirectMemory,
            TransferClass::D2DDirect,
        )
        .expect("measured direct case remains report data");
        let staged_phase = CounterPhaseReport::available(
            CounterPhase::ExplicitStagedMemory,
            4096,
            vec![
                EventRole::IioDataReqOfCpuMemReadAllParts,
                EventRole::IioDataReqOfCpuMemWriteAllParts,
            ],
            Vec::new(),
            Vec::new(),
            CounterRunReport::new(Duration::from_millis(20), Vec::new(), Vec::new(), None),
        );
        let phases = vec![staged_phase];
        let analysis = synthesize(&VerdictInput {
            cpu_supported: true,
            source: &staged_devices[0],
            destination: &staged_devices[1],
            direct: &direct,
            staged: &staged,
            phases: &phases,
            acs_redirect: false,
            acs_state_changed: false,
        });

        assert!(matches!(staged.outcome, CaseOutcome::Skipped { .. }));
        assert!(matches!(direct.outcome, CaseOutcome::Measured { .. }));
        assert!(staged_phase_samples_are_empty(&phases[0]));
        assert_eq!(analysis.verdict, DiagnosticVerdict::Indeterminate);
        assert!(
            analysis
                .reasons
                .iter()
                .any(|reason| { reason.code() == EvidenceReasonCode::InsufficientCounterRepeats })
        );
    }

    #[test]
    fn skipped_direct_case_is_retained() {
        let report = report_with_case(
            vec![
                device(0, Some("0000:17:00.0")),
                device(1, Some("0000:18:00.0")),
            ],
            skipped_case(TransferClass::D2DDirect, "direct unavailable"),
        );

        let (case, _) = extract_case(report, CounterPhase::DirectMemory, TransferClass::D2DDirect)
            .expect("skipped direct case remains report data");

        assert!(matches!(case.outcome, CaseOutcome::Skipped { .. }));
    }

    #[test]
    fn topology_identity_rejects_bdf_drift_and_missing_addresses() {
        let canonical = CanonicalPair {
            source: device(0, Some("0000:17:00.0")),
            destination: device(1, Some("0000:18:00.0")),
        };
        let matching = vec![canonical.source.clone(), canonical.destination.clone()];
        assert!(validate_topology_identity(&matching, &canonical).is_ok());

        let drifted = vec![
            device(0, Some("0000:19:00.0")),
            canonical.destination.clone(),
        ];
        let failure =
            validate_topology_identity(&drifted, &canonical).expect_err("BDF drift must fail");
        assert_eq!(failure.kind(), EvidenceFailureKind::TopologyMismatch);

        let mut wrong_endpoints = skipped_case(TransferClass::D2DStaged, "test");
        wrong_endpoints.source = Endpoint::Device(2);
        let failure = validate_repeated_pair_identity(&wrong_endpoints, &matching, &canonical)
            .expect_err("endpoint index drift must fail");
        assert_eq!(failure.kind(), EvidenceFailureKind::TopologyMismatch);

        let missing = CanonicalPair {
            source: device(0, None),
            destination: canonical.destination,
        };
        let failure = validate_topology_identity(
            &[missing.source.clone(), missing.destination.clone()],
            &missing,
        )
        .expect_err("missing canonical BDF must fail");
        assert_eq!(failure.kind(), EvidenceFailureKind::TopologyMismatch);
    }

    fn staged_phase_samples_are_empty(phase: &CounterPhaseReport) -> bool {
        phase
            .available_run()
            .is_some_and(|run| run.sample_windows().is_empty())
    }

    fn report_with_case(devices: Vec<DeviceInfo>, case: BenchCase) -> BenchReport {
        BenchReport {
            system: SystemInfo {
                host: HostInfo {
                    cpu_model: "test".to_owned(),
                    logical_cpus: 1,
                    physical_cores: Some(1),
                    sockets: Some(1),
                },
                devices,
            },
            cases: vec![case],
        }
    }

    fn skipped_case(class: TransferClass, reason: &str) -> BenchCase {
        let operation = match class {
            TransferClass::D2DDirect => Operation::Direct {
                peer_access: PeerAccess::Yes,
                route: PeerRoute::Unknown("test".to_owned()),
            },
            TransferClass::D2DStaged => Operation::ExplicitStaged {
                route: PeerRoute::Unknown("test".to_owned()),
            },
            TransferClass::H2D | TransferClass::D2H | TransferClass::D2DSameDevice => {
                panic!("test helper only supports cross-device cases")
            }
        };
        BenchCase {
            mode: BenchMode::Saturation,
            selected_group: None,
            streams: Vec::new(),
            second_phase_streams: Vec::new(),
            verification_stream: None,
            transfer_class: class,
            operation,
            source: Endpoint::Device(0),
            destination: Endpoint::Device(1),
            byte_count: 4096,
            allocation: if class == TransferClass::D2DStaged {
                AllocationKind::PinnedStaging
            } else {
                AllocationKind::Device
            },
            timing: TimingMode::WallClock,
            warmup: Duration::ZERO,
            requested_samples: MIN_SAMPLES,
            pcie_link: LinkInfo::Unknown {
                reason: "cross-device".to_owned(),
            },
            outcome: CaseOutcome::Skipped {
                reason: reason.to_owned(),
            },
        }
    }

    fn measured_direct_case(median: f64) -> BenchCase {
        let mut case = skipped_case(TransferClass::D2DDirect, "unused");
        let summary = test_summary(median);
        case.outcome = CaseOutcome::Measured {
            time_summary: Box::new(test_summary(1.0)),
            summary,
            samples_gb_s: vec![median; usize::try_from(MIN_SAMPLES).expect("sample count")],
        };
        case
    }

    fn test_summary(median: f64) -> Summary {
        Summary {
            count: usize::try_from(MIN_SAMPLES).expect("sample count"),
            median,
            median_confidence: ConfidenceInterval {
                confidence_level: 0.95,
                lower_bound: median,
                upper_bound: median,
                resamples: 1,
            },
            mad: 0.0,
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
            shape: crate::stats::DistributionShape::Ordinary,
        }
    }

    fn device(index: u32, pci_address: Option<&str>) -> DeviceInfo {
        DeviceInfo {
            index,
            name: format!("dev{index}"),
            pci_address: pci_address.map(str::to_owned),
            pcie_link: LinkInfo::Unknown {
                reason: "test".to_owned(),
            },
            queue_groups: Vec::new(),
        }
    }
}
