use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::time::{Duration, Instant};

use super::error::{CounterFailure, CounterTimingContext, EvidenceError, Result};
use super::intel_perfmon::EventRole;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct EvidenceRunId(u64);

impl EvidenceRunId {
    pub fn new(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct CounterId(u32);

impl CounterId {
    pub fn new(value: u32) -> Self {
        Self(value)
    }
}

impl fmt::Display for CounterId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "counter{}", self.0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CounterReading {
    id: CounterId,
    role: EventRole,
    value: u64,
    time_enabled: Duration,
    time_running: Duration,
}

impl CounterReading {
    pub fn new(
        id: CounterId,
        role: EventRole,
        value: u64,
        time_enabled: Duration,
        time_running: Duration,
    ) -> Result<Self> {
        validate_timing(CounterTimingContext::Reading, time_enabled, time_running)?;
        Ok(Self {
            id,
            role,
            value,
            time_enabled,
            time_running,
        })
    }

    pub fn id(&self) -> CounterId {
        self.id
    }

    pub fn role(&self) -> EventRole {
        self.role
    }

    pub fn value(&self) -> u64 {
        self.value
    }

    pub fn time_enabled(&self) -> Duration {
        self.time_enabled
    }

    pub fn time_running(&self) -> Duration {
        self.time_running
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CounterSnapshot {
    run: EvidenceRunId,
    taken_at: Instant,
    readings: Vec<CounterReading>,
}

impl CounterSnapshot {
    pub fn new(
        run: EvidenceRunId,
        taken_at: Instant,
        readings: Vec<CounterReading>,
    ) -> Result<Self> {
        validate_unique_readings(&readings)?;
        Ok(Self {
            run,
            taken_at,
            readings,
        })
    }

    pub fn run(&self) -> EvidenceRunId {
        self.run
    }

    pub fn taken_at(&self) -> Instant {
        self.taken_at
    }

    pub fn readings(&self) -> &[CounterReading] {
        &self.readings
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CounterDelta {
    id: CounterId,
    role: EventRole,
    value: u64,
    time_enabled: Duration,
    time_running: Duration,
}

impl CounterDelta {
    pub fn id(&self) -> CounterId {
        self.id
    }

    pub fn role(&self) -> EventRole {
        self.role
    }

    pub fn value(&self) -> u64 {
        self.value
    }

    pub fn time_enabled(&self) -> Duration {
        self.time_enabled
    }

    pub fn time_running(&self) -> Duration {
        self.time_running
    }

    pub fn attributable_value(
        &self,
        policy: MultiplexingPolicy,
    ) -> Result<AttributableCounterValue> {
        if self.time_running.is_zero() {
            return Err(counter_error(CounterFailure::ZeroRunning {
                id: self.id.to_string(),
            }));
        }

        if self.time_enabled == self.time_running {
            return Ok(AttributableCounterValue {
                id: self.id,
                role: self.role,
                raw_value: self.value,
                scaling: CounterScaling::None,
            });
        }

        match policy {
            MultiplexingPolicy::RequireFullRunning => {
                Err(counter_error(CounterFailure::MultiplexingRefused {
                    id: self.id.to_string(),
                }))
            }
            MultiplexingPolicy::AcknowledgeScaling => Ok(AttributableCounterValue {
                id: self.id,
                role: self.role,
                raw_value: self.value,
                scaling: CounterScaling::Scaled {
                    time_enabled: self.time_enabled,
                    time_running: self.time_running,
                },
            }),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MultiplexingPolicy {
    RequireFullRunning,
    AcknowledgeScaling,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CounterScaling {
    None,
    Scaled {
        time_enabled: Duration,
        time_running: Duration,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AttributableCounterValue {
    id: CounterId,
    role: EventRole,
    raw_value: u64,
    scaling: CounterScaling,
}

impl AttributableCounterValue {
    pub fn id(self) -> CounterId {
        self.id
    }

    pub fn role(self) -> EventRole {
        self.role
    }

    pub fn raw_value(self) -> u64 {
        self.raw_value
    }

    pub fn scaling(self) -> CounterScaling {
        self.scaling
    }

    /// Returns a display approximation. Evidence logic should use `raw_value`
    /// and require `CounterScaling::None`.
    pub fn approximate_value(self) -> f64 {
        match self.scaling {
            CounterScaling::None => self.raw_value as f64,
            CounterScaling::Scaled {
                time_enabled,
                time_running,
            } => self.raw_value as f64 * time_enabled.as_secs_f64() / time_running.as_secs_f64(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CounterDeltas {
    elapsed: Duration,
    deltas: Vec<CounterDelta>,
}

impl CounterDeltas {
    pub fn elapsed(&self) -> Duration {
        self.elapsed
    }

    pub fn deltas(&self) -> &[CounterDelta] {
        &self.deltas
    }
}

pub trait CounterSource {
    fn snapshot(&mut self) -> Result<CounterSnapshot>;
}

#[derive(Clone, Debug)]
pub struct SyntheticCounterSource {
    snapshots: Vec<CounterSnapshot>,
    next: usize,
}

impl SyntheticCounterSource {
    pub fn new(snapshots: Vec<CounterSnapshot>) -> Self {
        Self { snapshots, next: 0 }
    }
}

impl CounterSource for SyntheticCounterSource {
    fn snapshot(&mut self) -> Result<CounterSnapshot> {
        let Some(snapshot) = self.snapshots.get(self.next).cloned() else {
            return Err(counter_error(CounterFailure::SyntheticSourceExhausted));
        };
        self.next += 1;
        Ok(snapshot)
    }
}

pub fn diff_snapshots(before: &CounterSnapshot, after: &CounterSnapshot) -> Result<CounterDeltas> {
    if before.run != after.run {
        return Err(counter_error(CounterFailure::RunMismatch));
    }

    let Some(elapsed) = after.taken_at.checked_duration_since(before.taken_at) else {
        return Err(counter_error(CounterFailure::TimeOrder));
    };

    let before_by_id = readings_by_id(before)?;
    let after_by_id = readings_by_id(after)?;
    if before_by_id.keys().ne(after_by_id.keys()) {
        return Err(counter_error(CounterFailure::CounterSetMismatch));
    }

    let mut deltas = Vec::with_capacity(before_by_id.len());
    for (id, before) in before_by_id {
        let after = after_by_id
            .get(&id)
            .expect("matching keys were checked before lookup");
        if before.role != after.role {
            return Err(counter_error(CounterFailure::RoleChanged {
                id: id.to_string(),
                before: before.role.to_string(),
                after: after.role.to_string(),
            }));
        }
        let Some(value) = after.value.checked_sub(before.value) else {
            return Err(counter_error(CounterFailure::ValueRegression {
                id: id.to_string(),
            }));
        };
        let Some(time_enabled) = after.time_enabled.checked_sub(before.time_enabled) else {
            return Err(counter_error(CounterFailure::TimeEnabledRegression {
                id: id.to_string(),
            }));
        };
        let Some(time_running) = after.time_running.checked_sub(before.time_running) else {
            return Err(counter_error(CounterFailure::TimeRunningRegression {
                id: id.to_string(),
            }));
        };
        validate_timing(CounterTimingContext::Delta, time_enabled, time_running)?;
        deltas.push(CounterDelta {
            id,
            role: after.role,
            value,
            time_enabled,
            time_running,
        });
    }

    Ok(CounterDeltas { elapsed, deltas })
}

fn validate_timing(
    context: CounterTimingContext,
    time_enabled: Duration,
    time_running: Duration,
) -> Result<()> {
    if time_running > time_enabled {
        return Err(counter_error(CounterFailure::InvalidTiming {
            context,
            time_enabled,
            time_running,
        }));
    }
    Ok(())
}

fn validate_unique_readings(readings: &[CounterReading]) -> Result<()> {
    let mut seen = BTreeSet::new();
    for reading in readings {
        if !seen.insert(reading.id()) {
            return Err(counter_error(CounterFailure::DuplicateCounterId {
                id: reading.id().to_string(),
            }));
        }
    }
    Ok(())
}

fn readings_by_id(snapshot: &CounterSnapshot) -> Result<BTreeMap<CounterId, &CounterReading>> {
    let mut readings = BTreeMap::new();
    for reading in snapshot.readings() {
        if readings.insert(reading.id(), reading).is_some() {
            return Err(counter_error(CounterFailure::DuplicateCounterId {
                id: reading.id().to_string(),
            }));
        }
    }
    Ok(readings)
}

fn counter_error(reason: CounterFailure) -> EvidenceError {
    EvidenceError::Counter(reason)
}

#[cfg(test)]
mod tests {
    use super::super::intel_perfmon::EventRole;
    use super::*;

    fn reading(id: u32, value: u64, at_millis: u64) -> CounterReading {
        CounterReading::new(
            CounterId::new(id),
            EventRole::IioDataReqOfCpuPeerWriteAllParts,
            value,
            Duration::from_millis(at_millis),
            Duration::from_millis(at_millis),
        )
        .expect("reading")
    }

    fn snapshot(base: Instant, at_millis: u64, values: &[(u32, u64)]) -> CounterSnapshot {
        let readings = values
            .iter()
            .map(|(id, value)| reading(*id, *value, at_millis))
            .collect();
        CounterSnapshot::new(
            EvidenceRunId::new(7),
            base + Duration::from_millis(at_millis),
            readings,
        )
        .expect("snapshot")
    }

    #[test]
    fn diffs_synthetic_snapshots_without_verdict() {
        let base = Instant::now();
        let before = snapshot(base, 100, &[(0, 10), (1, 20)]);
        let after = snapshot(base, 250, &[(0, 15), (1, 22)]);

        let deltas = diff_snapshots(&before, &after).expect("snapshot diff");

        assert_eq!(deltas.elapsed(), Duration::from_millis(150));
        assert_eq!(deltas.deltas()[0].value(), 5);
        assert_eq!(deltas.deltas()[1].value(), 2);
        assert_eq!(
            deltas.deltas()[0]
                .attributable_value(MultiplexingPolicy::RequireFullRunning)
                .expect("attributable")
                .raw_value(),
            5
        );
    }

    #[test]
    fn rejects_counter_set_mismatch() {
        let base = Instant::now();
        let before = snapshot(base, 100, &[(0, 10)]);
        let after = snapshot(base, 250, &[(1, 15)]);

        assert!(matches!(
            diff_snapshots(&before, &after),
            Err(EvidenceError::Counter(CounterFailure::CounterSetMismatch))
        ));
    }

    #[test]
    fn rejects_counter_rollover_for_plain_delta() {
        let base = Instant::now();
        let before = snapshot(base, 100, &[(0, 10)]);
        let after = snapshot(base, 250, &[(0, 9)]);

        assert!(matches!(
            diff_snapshots(&before, &after),
            Err(EvidenceError::Counter(
                CounterFailure::ValueRegression { .. }
            ))
        ));
    }

    #[test]
    fn rejects_run_mismatch_and_time_order() {
        let base = Instant::now();
        let before = snapshot(base, 100, &[(0, 10)]);
        let other_run = CounterSnapshot::new(
            EvidenceRunId::new(8),
            base + Duration::from_millis(250),
            vec![reading(0, 11, 250)],
        )
        .expect("snapshot");
        assert!(matches!(
            diff_snapshots(&before, &other_run),
            Err(EvidenceError::Counter(CounterFailure::RunMismatch))
        ));

        let earlier = snapshot(base, 90, &[(0, 11)]);
        assert!(matches!(
            diff_snapshots(&before, &earlier),
            Err(EvidenceError::Counter(CounterFailure::TimeOrder))
        ));
    }

    #[test]
    fn rejects_duplicate_id_and_role_change() {
        let base = Instant::now();
        let duplicate = CounterSnapshot::new(
            EvidenceRunId::new(7),
            base,
            vec![reading(0, 1, 1), reading(0, 2, 1)],
        );
        assert!(matches!(
            duplicate,
            Err(EvidenceError::Counter(
                CounterFailure::DuplicateCounterId { .. }
            ))
        ));

        let before = snapshot(base, 100, &[(0, 10)]);
        let after = CounterSnapshot::new(
            EvidenceRunId::new(7),
            base + Duration::from_millis(200),
            vec![
                CounterReading::new(
                    CounterId::new(0),
                    EventRole::UpiRxDataFlitsAll,
                    11,
                    Duration::from_millis(200),
                    Duration::from_millis(200),
                )
                .expect("reading"),
            ],
        )
        .expect("snapshot");
        assert!(matches!(
            diff_snapshots(&before, &after),
            Err(EvidenceError::Counter(CounterFailure::RoleChanged { .. }))
        ));
    }

    #[test]
    fn rejects_time_regressions_and_invalid_timing() {
        assert!(matches!(
            CounterReading::new(
                CounterId::new(0),
                EventRole::IioDataReqOfCpuPeerWriteAllParts,
                0,
                Duration::from_millis(1),
                Duration::from_millis(2),
            ),
            Err(EvidenceError::Counter(CounterFailure::InvalidTiming {
                context: CounterTimingContext::Reading,
                ..
            }))
        ));

        let base = Instant::now();
        let before = snapshot(base, 100, &[(0, 10)]);
        let time_enabled_regressed = CounterSnapshot::new(
            EvidenceRunId::new(7),
            base + Duration::from_millis(200),
            vec![
                CounterReading::new(
                    CounterId::new(0),
                    EventRole::IioDataReqOfCpuPeerWriteAllParts,
                    11,
                    Duration::from_millis(99),
                    Duration::from_millis(99),
                )
                .expect("reading"),
            ],
        )
        .expect("snapshot");
        assert!(matches!(
            diff_snapshots(&before, &time_enabled_regressed),
            Err(EvidenceError::Counter(
                CounterFailure::TimeEnabledRegression { .. }
            ))
        ));

        let time_running_regressed = CounterSnapshot::new(
            EvidenceRunId::new(7),
            base + Duration::from_millis(200),
            vec![
                CounterReading::new(
                    CounterId::new(0),
                    EventRole::IioDataReqOfCpuPeerWriteAllParts,
                    11,
                    Duration::from_millis(200),
                    Duration::from_millis(99),
                )
                .expect("reading"),
            ],
        )
        .expect("snapshot");
        assert!(matches!(
            diff_snapshots(&before, &time_running_regressed),
            Err(EvidenceError::Counter(
                CounterFailure::TimeRunningRegression { .. }
            ))
        ));

        let before_delta_invalid = CounterSnapshot::new(
            EvidenceRunId::new(7),
            base + Duration::from_millis(100),
            vec![
                CounterReading::new(
                    CounterId::new(0),
                    EventRole::IioDataReqOfCpuPeerWriteAllParts,
                    10,
                    Duration::from_millis(100),
                    Duration::from_millis(90),
                )
                .expect("reading"),
            ],
        )
        .expect("snapshot");
        let delta_invalid = CounterSnapshot::new(
            EvidenceRunId::new(7),
            base + Duration::from_millis(200),
            vec![
                CounterReading::new(
                    CounterId::new(0),
                    EventRole::IioDataReqOfCpuPeerWriteAllParts,
                    11,
                    Duration::from_millis(150),
                    Duration::from_millis(150),
                )
                .expect("reading"),
            ],
        )
        .expect("snapshot");
        assert!(matches!(
            diff_snapshots(&before_delta_invalid, &delta_invalid),
            Err(EvidenceError::Counter(CounterFailure::InvalidTiming {
                context: CounterTimingContext::Delta,
                ..
            }))
        ));
    }

    #[test]
    fn requires_acknowledgement_for_multiplexed_attribution() {
        let delta = CounterDelta {
            id: CounterId::new(0),
            role: EventRole::IioDataReqOfCpuPeerWriteAllParts,
            value: 10,
            time_enabled: Duration::from_millis(100),
            time_running: Duration::from_millis(50),
        };

        assert!(matches!(
            delta.attributable_value(MultiplexingPolicy::RequireFullRunning),
            Err(EvidenceError::Counter(
                CounterFailure::MultiplexingRefused { .. }
            ))
        ));
        let value = delta
            .attributable_value(MultiplexingPolicy::AcknowledgeScaling)
            .expect("scaled value");
        assert_eq!(value.raw_value(), 10);
        assert_eq!(value.approximate_value(), 20.0);
        assert!(matches!(value.scaling(), CounterScaling::Scaled { .. }));
    }

    #[test]
    fn unscaled_attribution_preserves_values_above_f64_integer_precision() {
        let exact = (1_u64 << 53) + 1;
        let delta = CounterDelta {
            id: CounterId::new(1),
            role: EventRole::IioDataReqOfCpuPeerWriteAllParts,
            value: exact,
            time_enabled: Duration::from_millis(1),
            time_running: Duration::from_millis(1),
        };

        let attributed = delta
            .attributable_value(MultiplexingPolicy::RequireFullRunning)
            .expect("attributable");

        assert_eq!(attributed.raw_value(), exact);
        assert_eq!(attributed.scaling(), CounterScaling::None);
    }

    #[test]
    fn rejects_zero_running_time_for_attribution() {
        let delta = CounterDelta {
            id: CounterId::new(0),
            role: EventRole::IioDataReqOfCpuPeerWriteAllParts,
            value: 10,
            time_enabled: Duration::from_millis(100),
            time_running: Duration::ZERO,
        };

        assert!(matches!(
            delta.attributable_value(MultiplexingPolicy::AcknowledgeScaling),
            Err(EvidenceError::Counter(CounterFailure::ZeroRunning { .. }))
        ));
    }

    #[test]
    fn synthetic_source_reports_exhaustion() {
        let mut source = SyntheticCounterSource::new(Vec::new());
        assert!(matches!(
            source.snapshot(),
            Err(EvidenceError::Counter(
                CounterFailure::SyntheticSourceExhausted
            ))
        ));
    }
}
