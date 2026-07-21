use std::thread;
use std::time::Duration;

use crate::benchmark::measurement::{CaseExecutionError, MeasurementObserver, SampleContext};
use crate::cli::TransferClass;
use crate::evidence::counters::{CounterDeltas, CounterId, EvidenceRunId, diff_snapshots};
use crate::evidence::intel_perfmon::{self, EventRole, PerfmonProfile};
use crate::evidence::{
    CounterDelta, CounterSnapshot, CpuProfile, LinuxPerfCounterSet, LinuxPerfEventSpec,
    LinuxPerfGroupSpec, LinuxPerfMeasurement, PmuInstance, PmuKind,
};

use super::model::{
    CounterEventReport, CounterGroupReport, CounterPhase, CounterPhaseReport, CounterRunReport,
    CounterSample, CounterWindow, EvidenceAvailability, EvidenceFailure, MissingCounterRole,
    RoleCounterTotal,
};

pub(crate) const IDLE_BASELINE_WINDOWS: usize = 5;
pub(crate) const IDLE_BASELINE_DURATION: Duration = Duration::from_millis(20);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MissingRolePolicy {
    Fail,
    Skip,
}

#[derive(Debug)]
pub(crate) struct PhaseCounterPreparation {
    pub(crate) phase: CounterPhase,
    pub(crate) expected_transfer_bytes: u64,
    pub(crate) roles: Vec<EventRole>,
    pub(crate) groups: Vec<CounterGroupReport>,
    pub(crate) missing_roles: Vec<MissingCounterRole>,
    pub(crate) counter_set: Option<LinuxPerfCounterSet>,
    pub(crate) failure: Option<EvidenceFailure>,
}

impl PhaseCounterPreparation {
    pub(crate) fn unavailable_report(self) -> CounterPhaseReport {
        CounterPhaseReport::unavailable(
            self.phase,
            self.expected_transfer_bytes,
            self.roles,
            self.groups,
            self.missing_roles,
            self.failure
                .unwrap_or_else(|| EvidenceFailure::other("counter phase was unavailable")),
        )
    }

    pub(crate) fn available_report(
        self,
        baseline_windows: Vec<CounterWindow>,
        sample_windows: Vec<CounterSample>,
        terminal_failure: Option<EvidenceFailure>,
    ) -> CounterPhaseReport {
        CounterPhaseReport::available(
            self.phase,
            self.expected_transfer_bytes,
            self.roles,
            self.groups,
            self.missing_roles,
            CounterRunReport::new(
                IDLE_BASELINE_DURATION,
                baseline_windows,
                sample_windows,
                terminal_failure,
            ),
        )
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare_phase(
    phase: CounterPhase,
    expected_transfer_bytes: u64,
    profile: Option<CpuProfile>,
    pmus: &EvidenceAvailability<Vec<PmuInstance>>,
    pmu_kind: PmuKind,
    requested_roles: &[EventRole],
    missing_role_policy: MissingRolePolicy,
    run_id: EvidenceRunId,
    next_counter_id: &mut u32,
) -> PhaseCounterPreparation {
    let mut preparation = PhaseCounterPreparation {
        phase,
        expected_transfer_bytes,
        roles: Vec::new(),
        groups: Vec::new(),
        missing_roles: Vec::new(),
        counter_set: None,
        failure: None,
    };

    let Some(profile) = profile else {
        preparation.failure = Some(EvidenceFailure::unsupported(
            "supported Intel CPU/profile is required for counter-consistent evidence",
        ));
        return preparation;
    };
    let perfmon_profile = intel_perfmon::profile(profile.perfmon_profile());

    let pmus = match pmus {
        EvidenceAvailability::Available(pmus) => pmus,
        EvidenceAvailability::PermissionDenied(failure)
        | EvidenceAvailability::Unsupported(failure)
        | EvidenceAvailability::Malformed(failure)
        | EvidenceAvailability::ResourceUnavailable(failure)
        | EvidenceAvailability::Io(failure)
        | EvidenceAvailability::Other(failure) => {
            preparation.failure = Some(failure.clone());
            return preparation;
        }
    };

    if pmus.iter().any(|pmu| pmu.kind() != pmu_kind) {
        preparation.failure = Some(EvidenceFailure::malformed(format!(
            "{pmu_kind} counter phase received mismatched PMU kind"
        )));
        return preparation;
    }

    let events = match available_events(perfmon_profile, requested_roles, missing_role_policy) {
        Ok((events, missing_roles)) => {
            preparation.missing_roles = missing_roles;
            events
        }
        Err(failure) => {
            preparation.failure = Some(failure);
            return preparation;
        }
    };

    if events.is_empty() {
        preparation.failure = Some(EvidenceFailure::unsupported(
            "no requested counter roles are available for this profile",
        ));
        return preparation;
    }
    preparation.roles = events.iter().map(|event| event.role).collect();

    let mut group_specs = Vec::new();
    for pmu in pmus {
        let mut event_specs = Vec::new();
        let mut event_reports = Vec::new();
        for event in &events {
            let packed = match event.encode_for_linux(pmu.format()) {
                Ok(packed) => packed,
                Err(error) => {
                    preparation.failure = Some(error.into());
                    return preparation;
                }
            };
            let id = CounterId::new(*next_counter_id);
            *next_counter_id = next_counter_id.saturating_add(1);
            let spec = match LinuxPerfEventSpec::new(id, event.role, pmu, packed) {
                Ok(spec) => spec,
                Err(error) => {
                    preparation.failure = Some(error.into());
                    return preparation;
                }
            };
            event_reports.push(CounterEventReport::new(
                id.to_string(),
                event.role,
                packed.config,
                packed.config1,
                packed.config2,
            ));
            event_specs.push(spec);
        }

        let group = match LinuxPerfGroupSpec::new(event_specs) {
            Ok(group) => group,
            Err(error) => {
                preparation.failure = Some(error.into());
                return preparation;
            }
        };
        preparation.groups.push(CounterGroupReport::new(
            group.pmu_name(),
            group.cpu(),
            event_reports,
        ));
        group_specs.push(group);
    }

    match LinuxPerfCounterSet::open(run_id, group_specs) {
        Ok(counter_set) => preparation.counter_set = Some(counter_set),
        Err(error) => preparation.failure = Some(error.into()),
    }
    preparation
}

fn available_events(
    profile: &'static PerfmonProfile,
    roles: &[EventRole],
    missing_role_policy: MissingRolePolicy,
) -> Result<
    (
        Vec<&'static intel_perfmon::PerfmonEvent>,
        Vec<MissingCounterRole>,
    ),
    EvidenceFailure,
> {
    let mut events = Vec::new();
    let mut missing = Vec::new();
    for role in roles {
        match profile.event(*role) {
            Ok(event) => events.push(event),
            Err(error) if missing_role_policy == MissingRolePolicy::Skip => {
                missing.push(MissingCounterRole::new(*role, error.to_string()));
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok((events, missing))
}

pub(crate) fn collect_idle_baselines(
    counter_set: &mut LinuxPerfCounterSet,
) -> Result<Vec<CounterWindow>, EvidenceFailure> {
    let mut windows = Vec::with_capacity(IDLE_BASELINE_WINDOWS);
    for _ in 0..IDLE_BASELINE_WINDOWS {
        let active = counter_set.begin_window().map_err(EvidenceFailure::from)?;
        thread::sleep(IDLE_BASELINE_DURATION);
        let (before, after) = active.finish().map_err(EvidenceFailure::from)?;
        windows.push(counter_window(before, after)?);
    }
    Ok(windows)
}

pub(crate) trait CounterWindowSet {
    fn measure_window(
        &mut self,
        operation: &mut dyn FnMut() -> Result<Duration, CaseExecutionError>,
    ) -> (
        Result<Duration, CaseExecutionError>,
        Result<CounterWindow, EvidenceFailure>,
    );
}

impl CounterWindowSet for LinuxPerfCounterSet {
    fn measure_window(
        &mut self,
        operation: &mut dyn FnMut() -> Result<Duration, CaseExecutionError>,
    ) -> (
        Result<Duration, CaseExecutionError>,
        Result<CounterWindow, EvidenceFailure>,
    ) {
        let (operation_result, evidence_result) = self.measure(operation);
        (
            operation_result,
            evidence_result
                .map_err(EvidenceFailure::from)
                .and_then(counter_window_from_measurement),
        )
    }
}

pub(crate) struct CounterPhaseObserver<S> {
    counter_set: Option<S>,
    sample_windows: Vec<CounterSample>,
    terminal_failure: Option<EvidenceFailure>,
}

impl<S> CounterPhaseObserver<S> {
    pub(crate) fn new(counter_set: S) -> Self {
        Self {
            counter_set: Some(counter_set),
            sample_windows: Vec::new(),
            terminal_failure: None,
        }
    }

    pub(crate) fn into_parts(self) -> (Vec<CounterSample>, Option<EvidenceFailure>) {
        (self.sample_windows, self.terminal_failure)
    }
}

impl<S> MeasurementObserver for CounterPhaseObserver<S>
where
    S: CounterWindowSet,
{
    fn observe(
        &mut self,
        context: &SampleContext,
        operation: &mut dyn FnMut() -> Result<Duration, CaseExecutionError>,
    ) -> Result<Duration, CaseExecutionError> {
        let Some(counter_set) = self.counter_set.as_mut() else {
            return operation();
        };

        let (operation_result, evidence_result) = counter_set.measure_window(operation);
        match evidence_result {
            Ok(window) => {
                debug_assert!(matches!(
                    context.transfer_class,
                    TransferClass::D2DDirect | TransferClass::D2DStaged
                ));
                self.sample_windows
                    .push(CounterSample::new(context.zero_based_sample_index, window));
            }
            Err(failure) => {
                self.terminal_failure = Some(failure);
                self.counter_set = None;
            }
        }
        operation_result
    }
}

pub(crate) fn counter_window_from_measurement(
    measurement: LinuxPerfMeasurement,
) -> Result<CounterWindow, EvidenceFailure> {
    let (before, after) = measurement.into_parts();
    counter_window(before, after)
}

fn counter_window(
    before: CounterSnapshot,
    after: CounterSnapshot,
) -> Result<CounterWindow, EvidenceFailure> {
    let deltas = diff_snapshots(&before, &after).map_err(EvidenceFailure::from)?;
    counter_window_from_deltas(before, after, &deltas)
}

fn counter_window_from_deltas(
    before: CounterSnapshot,
    after: CounterSnapshot,
    deltas: &CounterDeltas,
) -> Result<CounterWindow, EvidenceFailure> {
    let role_totals = aggregate_roles(deltas.deltas())?;
    Ok(CounterWindow::new(
        before,
        after,
        deltas.elapsed(),
        deltas.deltas().to_vec(),
        role_totals,
    ))
}

fn aggregate_roles(deltas: &[CounterDelta]) -> Result<Vec<RoleCounterTotal>, EvidenceFailure> {
    let mut totals = Vec::<RoleCounterTotal>::new();
    for delta in deltas {
        if let Some(total) = totals.iter_mut().find(|total| total.role() == delta.role()) {
            let value = total.value().checked_add(delta.value()).ok_or_else(|| {
                EvidenceFailure::counter_overflow(format!(
                    "counter aggregation overflow for {}",
                    delta.role()
                ))
            })?;
            *total = RoleCounterTotal::new(delta.role(), value);
        } else {
            totals.push(RoleCounterTotal::new(delta.role(), delta.value()));
        }
    }
    Ok(totals)
}

pub(crate) fn required_memory_roles() -> [EventRole; 2] {
    [
        EventRole::IioDataReqOfCpuMemReadAllParts,
        EventRole::IioDataReqOfCpuMemWriteAllParts,
    ]
}

pub(crate) fn peer_write_roles() -> [EventRole; 2] {
    [
        EventRole::IioDataReqOfCpuPeerWriteAllParts,
        EventRole::IioDataReqByCpuPeerWriteAllParts,
    ]
}

pub(crate) fn peer_read_roles() -> [EventRole; 2] {
    [
        EventRole::IioDataReqOfCpuPeerReadAllParts,
        EventRole::IioDataReqByCpuPeerReadAllParts,
    ]
}

pub(crate) fn upi_roles() -> [EventRole; 2] {
    [EventRole::UpiTxDataFlitsAll, EventRole::UpiRxDataFlitsAll]
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::time::{Duration, Instant};

    use crate::benchmark::measurement::{MeasurementObserver, sample_context};
    use crate::cli::BenchMode;
    use crate::evidence::counters::{CounterReading, CounterSnapshot};

    use super::*;

    #[derive(Default)]
    struct FailingSet {
        calls: Cell<u32>,
    }

    impl CounterWindowSet for FailingSet {
        fn measure_window(
            &mut self,
            operation: &mut dyn FnMut() -> Result<Duration, CaseExecutionError>,
        ) -> (
            Result<Duration, CaseExecutionError>,
            Result<CounterWindow, EvidenceFailure>,
        ) {
            self.calls.set(self.calls.get() + 1);
            (
                operation(),
                Err(EvidenceFailure::other("synthetic counter failure")),
            )
        }
    }

    #[test]
    fn observer_degrades_evidence_failure_without_skipping_operation() {
        let mut observer = CounterPhaseObserver::new(FailingSet::default());
        let mut operation_calls = 0;
        let context = sample_context(TransferClass::D2DDirect, 4096, 0, BenchMode::Saturation);

        let elapsed = observer
            .observe(&context, &mut || {
                operation_calls += 1;
                Ok(Duration::from_nanos(9))
            })
            .expect("operation succeeds");

        assert_eq!(elapsed, Duration::from_nanos(9));
        assert_eq!(operation_calls, 1);
        let (samples, failure) = observer.into_parts();
        assert!(samples.is_empty());
        assert!(failure.is_some());
    }

    #[test]
    fn observer_keeps_calling_operation_after_terminal_counter_failure() {
        let mut observer = CounterPhaseObserver::new(FailingSet::default());
        let context = sample_context(TransferClass::D2DDirect, 4096, 0, BenchMode::Saturation);
        let mut operation_calls = 0;

        let _ = observer.observe(&context, &mut || {
            operation_calls += 1;
            Ok(Duration::from_nanos(1))
        });
        let _ = observer.observe(&context, &mut || {
            operation_calls += 1;
            Ok(Duration::from_nanos(2))
        });

        assert_eq!(operation_calls, 2);
    }

    #[test]
    fn role_aggregation_overflow_is_explicit() {
        let started = Instant::now();
        let role = EventRole::IioDataReqOfCpuMemReadAllParts;
        let before = CounterSnapshot::new(
            EvidenceRunId::new(1),
            started,
            vec![
                CounterReading::new(CounterId::new(0), role, 0, Duration::ZERO, Duration::ZERO)
                    .expect("reading"),
                CounterReading::new(CounterId::new(1), role, 0, Duration::ZERO, Duration::ZERO)
                    .expect("reading"),
            ],
        )
        .expect("snapshot");
        let after = CounterSnapshot::new(
            EvidenceRunId::new(1),
            started + Duration::from_millis(1),
            vec![
                CounterReading::new(
                    CounterId::new(0),
                    role,
                    u64::MAX,
                    Duration::from_millis(1),
                    Duration::from_millis(1),
                )
                .expect("reading"),
                CounterReading::new(
                    CounterId::new(1),
                    role,
                    1,
                    Duration::from_millis(1),
                    Duration::from_millis(1),
                )
                .expect("reading"),
            ],
        )
        .expect("snapshot");

        let failure = counter_window(before, after).expect_err("aggregate must overflow");

        assert_eq!(
            failure.kind(),
            super::super::model::EvidenceFailureKind::CounterOverflow
        );
        assert!(failure.message().contains("aggregation overflow"));
    }
}
