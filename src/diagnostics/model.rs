use std::time::Duration;
use std::{fmt, io};

use crate::benchmark::BenchmarkError;
use crate::evidence::intel_perfmon::{EventRole, PerfmonAttribution};
use crate::evidence::{
    AcsBridgePathEvidence, CounterDelta, CounterSnapshot, CpuIdentity, CpuProfile, EvidenceError,
};
use crate::output::{BenchCase, DeviceInfo, LinkInfo, PeerAccess, PeerRoute};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct P2pDiagnosticOptions {
    source_device: u32,
    destination_device: u32,
    size_bytes: u64,
    samples: u32,
    warmup: Duration,
    queue_group: Option<u32>,
}

impl P2pDiagnosticOptions {
    pub fn new(
        source_device: u32,
        destination_device: u32,
        size_bytes: u64,
        samples: u32,
        warmup: Duration,
    ) -> Self {
        Self {
            source_device,
            destination_device,
            size_bytes,
            samples,
            warmup,
            queue_group: None,
        }
    }

    #[must_use]
    pub fn with_queue_group(mut self, queue_group: u32) -> Self {
        self.queue_group = Some(queue_group);
        self
    }

    pub fn source_device(&self) -> u32 {
        self.source_device
    }

    pub fn destination_device(&self) -> u32 {
        self.destination_device
    }

    pub fn size_bytes(&self) -> u64 {
        self.size_bytes
    }

    pub fn samples(&self) -> u32 {
        self.samples
    }

    pub fn warmup(&self) -> Duration {
        self.warmup
    }

    pub fn queue_group(&self) -> Option<u32> {
        self.queue_group
    }
}

#[derive(Debug)]
pub enum DiagnosticError {
    InvalidOptions(String),
    Benchmark(BenchmarkError),
    Reporter(io::Error),
    UnexpectedCaseCount { phase: CounterPhase, count: usize },
    UnexpectedCaseShape { phase: CounterPhase, reason: String },
    MissingDevice { index: u32 },
}

impl fmt::Display for DiagnosticError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidOptions(message) => f.write_str(message),
            Self::Benchmark(error) => write!(f, "{error}"),
            Self::Reporter(error) => write!(f, "diagnostic reporter failed: {error}"),
            Self::UnexpectedCaseCount { phase, count } => {
                write!(f, "{phase} produced {count} benchmark cases, expected one")
            }
            Self::UnexpectedCaseShape { phase, reason } => {
                write!(
                    f,
                    "{phase} benchmark case is not the requested pair: {reason}"
                )
            }
            Self::MissingDevice { index } => write!(f, "dev{index} is missing from diagnostics"),
        }
    }
}

impl std::error::Error for DiagnosticError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Benchmark(error) => Some(error),
            Self::Reporter(error) => Some(error),
            Self::InvalidOptions(_)
            | Self::UnexpectedCaseCount { .. }
            | Self::UnexpectedCaseShape { .. }
            | Self::MissingDevice { .. } => None,
        }
    }
}

impl From<BenchmarkError> for DiagnosticError {
    fn from(error: BenchmarkError) -> Self {
        Self::Benchmark(error)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct P2pDiagnosticReport {
    source: DeviceInfo,
    destination: DeviceInfo,
    peer_access: PeerAccess,
    route: PeerRoute,
    direct: BenchCase,
    staged: BenchCase,
    cpu: CpuEvidence,
    phases: Vec<CounterPhaseReport>,
    acs: EvidenceAvailability<AcsBridgePathEvidence>,
    verdict: DiagnosticVerdict,
    reasons: Vec<EvidenceReason>,
    perfmon_attribution: PerfmonAttribution,
}

#[allow(clippy::too_many_arguments)]
impl P2pDiagnosticReport {
    pub(crate) fn new(
        source: DeviceInfo,
        destination: DeviceInfo,
        peer_access: PeerAccess,
        route: PeerRoute,
        direct: BenchCase,
        staged: BenchCase,
        cpu: CpuEvidence,
        phases: Vec<CounterPhaseReport>,
        acs: EvidenceAvailability<AcsBridgePathEvidence>,
        verdict: DiagnosticVerdict,
        reasons: Vec<EvidenceReason>,
        perfmon_attribution: PerfmonAttribution,
    ) -> Self {
        Self {
            source,
            destination,
            peer_access,
            route,
            direct,
            staged,
            cpu,
            phases,
            acs,
            verdict,
            reasons,
            perfmon_attribution,
        }
    }

    pub fn source(&self) -> &DeviceInfo {
        &self.source
    }

    pub fn destination(&self) -> &DeviceInfo {
        &self.destination
    }

    pub fn peer_access(&self) -> &PeerAccess {
        &self.peer_access
    }

    pub fn route(&self) -> &PeerRoute {
        &self.route
    }

    pub fn direct(&self) -> &BenchCase {
        &self.direct
    }

    pub fn staged(&self) -> &BenchCase {
        &self.staged
    }

    pub fn cpu(&self) -> &CpuEvidence {
        &self.cpu
    }

    pub fn phases(&self) -> &[CounterPhaseReport] {
        &self.phases
    }

    pub fn acs(&self) -> &EvidenceAvailability<AcsBridgePathEvidence> {
        &self.acs
    }

    pub fn verdict(&self) -> &DiagnosticVerdict {
        &self.verdict
    }

    pub fn reasons(&self) -> &[EvidenceReason] {
        &self.reasons
    }

    pub fn perfmon_attribution(&self) -> PerfmonAttribution {
        self.perfmon_attribution
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CpuEvidence {
    identity: EvidenceAvailability<CpuIdentity>,
    profile: EvidenceAvailability<CpuProfile>,
}

impl CpuEvidence {
    pub(crate) fn new(
        identity: EvidenceAvailability<CpuIdentity>,
        profile: EvidenceAvailability<CpuProfile>,
    ) -> Self {
        Self { identity, profile }
    }

    pub fn identity(&self) -> &EvidenceAvailability<CpuIdentity> {
        &self.identity
    }

    pub fn profile(&self) -> &EvidenceAvailability<CpuProfile> {
        &self.profile
    }

    pub(crate) fn profile_value(&self) -> Option<CpuProfile> {
        match self.profile {
            EvidenceAvailability::Available(profile) => Some(profile),
            EvidenceAvailability::PermissionDenied(_)
            | EvidenceAvailability::Unsupported(_)
            | EvidenceAvailability::Malformed(_)
            | EvidenceAvailability::ResourceUnavailable(_)
            | EvidenceAvailability::Io(_)
            | EvidenceAvailability::Other(_) => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EvidenceAvailability<T> {
    Available(T),
    PermissionDenied(EvidenceFailure),
    Unsupported(EvidenceFailure),
    Malformed(EvidenceFailure),
    ResourceUnavailable(EvidenceFailure),
    Io(EvidenceFailure),
    Other(EvidenceFailure),
}

impl<T> EvidenceAvailability<T> {
    pub fn failure(&self) -> Option<&EvidenceFailure> {
        match self {
            Self::Available(_) => None,
            Self::PermissionDenied(failure)
            | Self::Unsupported(failure)
            | Self::Malformed(failure)
            | Self::ResourceUnavailable(failure)
            | Self::Io(failure)
            | Self::Other(failure) => Some(failure),
        }
    }

    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available(_))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvidenceFailure {
    kind: EvidenceFailureKind,
    message: String,
}

impl EvidenceFailure {
    pub(crate) fn new(kind: EvidenceFailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub(crate) fn unsupported(message: impl Into<String>) -> Self {
        Self::new(EvidenceFailureKind::Unsupported, message)
    }

    pub(crate) fn malformed(message: impl Into<String>) -> Self {
        Self::new(EvidenceFailureKind::Malformed, message)
    }

    pub(crate) fn resource_unavailable(message: impl Into<String>) -> Self {
        Self::new(EvidenceFailureKind::ResourceUnavailable, message)
    }

    pub(crate) fn counter_overflow(message: impl Into<String>) -> Self {
        Self::new(EvidenceFailureKind::CounterOverflow, message)
    }

    pub(crate) fn topology_mismatch(message: impl Into<String>) -> Self {
        Self::new(EvidenceFailureKind::TopologyMismatch, message)
    }

    pub(crate) fn other(message: impl Into<String>) -> Self {
        Self::new(EvidenceFailureKind::Other, message)
    }

    pub fn kind(&self) -> EvidenceFailureKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl From<EvidenceError> for EvidenceFailure {
    fn from(error: EvidenceError) -> Self {
        let kind = classify_evidence_error(&error);
        Self::new(kind, error.to_string())
    }
}

fn classify_evidence_error(error: &EvidenceError) -> EvidenceFailureKind {
    match error {
        EvidenceError::UnsupportedCpu(_) | EvidenceError::MissingPmu { .. } => {
            EvidenceFailureKind::Unsupported
        }
        EvidenceError::PermissionDenied { .. } => EvidenceFailureKind::PermissionDenied,
        EvidenceError::MalformedSysfs { .. }
        | EvidenceError::UnavailableEvent { .. }
        | EvidenceError::InvalidEventEncoding { .. }
        | EvidenceError::MissingFormatField { .. }
        | EvidenceError::FieldValueTooLarge { .. }
        | EvidenceError::Counter(_) => EvidenceFailureKind::Malformed,
        EvidenceError::PerfEvent(reason) => classify_perf_event_failure(reason),
        EvidenceError::Io { .. } => EvidenceFailureKind::Io,
    }
}

fn classify_perf_event_failure(reason: &crate::evidence::PerfEventFailure) -> EvidenceFailureKind {
    match reason {
        crate::evidence::PerfEventFailure::PermissionDenied { .. } => {
            EvidenceFailureKind::PermissionDenied
        }
        crate::evidence::PerfEventFailure::Unsupported { .. } => EvidenceFailureKind::Unsupported,
        crate::evidence::PerfEventFailure::ResourceUnavailable { .. } => {
            EvidenceFailureKind::ResourceUnavailable
        }
        crate::evidence::PerfEventFailure::RejectedAttr { .. }
        | crate::evidence::PerfEventFailure::Malformed { .. } => EvidenceFailureKind::Malformed,
        crate::evidence::PerfEventFailure::Io { .. } => EvidenceFailureKind::Io,
        crate::evidence::PerfEventFailure::Multiple { failures, .. } => {
            classify_combined_failures(failures)
        }
    }
}

fn classify_combined_failures(failures: &[EvidenceError]) -> EvidenceFailureKind {
    let mut kinds = failures.iter().map(classify_evidence_error);
    let Some(first) = kinds.next() else {
        return EvidenceFailureKind::Other;
    };
    if kinds.all(|kind| kind == first) {
        first
    } else {
        EvidenceFailureKind::Other
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EvidenceFailureKind {
    PermissionDenied,
    Unsupported,
    Malformed,
    ResourceUnavailable,
    Io,
    Other,
    CounterOverflow,
    TopologyMismatch,
}

pub(crate) fn availability_from_failure<T>(failure: EvidenceFailure) -> EvidenceAvailability<T> {
    match failure.kind {
        EvidenceFailureKind::PermissionDenied => EvidenceAvailability::PermissionDenied(failure),
        EvidenceFailureKind::Unsupported => EvidenceAvailability::Unsupported(failure),
        EvidenceFailureKind::Malformed => EvidenceAvailability::Malformed(failure),
        EvidenceFailureKind::ResourceUnavailable => {
            EvidenceAvailability::ResourceUnavailable(failure)
        }
        EvidenceFailureKind::Io => EvidenceAvailability::Io(failure),
        EvidenceFailureKind::Other
        | EvidenceFailureKind::CounterOverflow
        | EvidenceFailureKind::TopologyMismatch => EvidenceAvailability::Other(failure),
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum CounterPhase {
    ExplicitStagedMemory,
    DirectMemory,
    DirectPeerWrite,
    DirectPeerRead,
    DirectUpi,
}

impl fmt::Display for CounterPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExplicitStagedMemory => f.write_str("explicit staged memory counter phase"),
            Self::DirectMemory => f.write_str("direct memory counter phase"),
            Self::DirectPeerWrite => f.write_str("direct peer-write counter phase"),
            Self::DirectPeerRead => f.write_str("direct peer-read counter phase"),
            Self::DirectUpi => f.write_str("direct UPI counter phase"),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CounterPhaseReport {
    phase: CounterPhase,
    expected_transfer_bytes: u64,
    roles: Vec<EventRole>,
    groups: Vec<CounterGroupReport>,
    missing_roles: Vec<MissingCounterRole>,
    evidence: EvidenceAvailability<CounterRunReport>,
}

impl CounterPhaseReport {
    pub(crate) fn available(
        phase: CounterPhase,
        expected_transfer_bytes: u64,
        roles: Vec<EventRole>,
        groups: Vec<CounterGroupReport>,
        missing_roles: Vec<MissingCounterRole>,
        run: CounterRunReport,
    ) -> Self {
        Self {
            phase,
            expected_transfer_bytes,
            roles,
            groups,
            missing_roles,
            evidence: EvidenceAvailability::Available(run),
        }
    }

    pub(crate) fn unavailable(
        phase: CounterPhase,
        expected_transfer_bytes: u64,
        roles: Vec<EventRole>,
        groups: Vec<CounterGroupReport>,
        missing_roles: Vec<MissingCounterRole>,
        failure: EvidenceFailure,
    ) -> Self {
        Self {
            phase,
            expected_transfer_bytes,
            roles,
            groups,
            missing_roles,
            evidence: availability_from_failure(failure),
        }
    }

    pub fn phase(&self) -> CounterPhase {
        self.phase
    }

    pub fn expected_transfer_bytes(&self) -> u64 {
        self.expected_transfer_bytes
    }

    pub fn roles(&self) -> &[EventRole] {
        &self.roles
    }

    pub fn groups(&self) -> &[CounterGroupReport] {
        &self.groups
    }

    pub fn missing_roles(&self) -> &[MissingCounterRole] {
        &self.missing_roles
    }

    pub fn evidence(&self) -> &EvidenceAvailability<CounterRunReport> {
        &self.evidence
    }

    pub(crate) fn available_run(&self) -> Option<&CounterRunReport> {
        match &self.evidence {
            EvidenceAvailability::Available(run) => Some(run),
            EvidenceAvailability::PermissionDenied(_)
            | EvidenceAvailability::Unsupported(_)
            | EvidenceAvailability::Malformed(_)
            | EvidenceAvailability::ResourceUnavailable(_)
            | EvidenceAvailability::Io(_)
            | EvidenceAvailability::Other(_) => None,
        }
    }

    pub(crate) fn invalidate(self, failure: EvidenceFailure) -> Self {
        Self::unavailable(
            self.phase,
            self.expected_transfer_bytes,
            self.roles,
            self.groups,
            self.missing_roles,
            failure,
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MissingCounterRole {
    role: EventRole,
    reason: String,
}

impl MissingCounterRole {
    pub(crate) fn new(role: EventRole, reason: impl Into<String>) -> Self {
        Self {
            role,
            reason: reason.into(),
        }
    }

    pub fn role(&self) -> EventRole {
        self.role
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CounterGroupReport {
    pmu_name: String,
    cpu: i32,
    events: Vec<CounterEventReport>,
}

impl CounterGroupReport {
    pub(crate) fn new(
        pmu_name: impl Into<String>,
        cpu: i32,
        events: Vec<CounterEventReport>,
    ) -> Self {
        Self {
            pmu_name: pmu_name.into(),
            cpu,
            events,
        }
    }

    pub fn pmu_name(&self) -> &str {
        &self.pmu_name
    }

    pub fn cpu(&self) -> i32 {
        self.cpu
    }

    pub fn events(&self) -> &[CounterEventReport] {
        &self.events
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CounterEventReport {
    counter_id: String,
    role: EventRole,
    config: u64,
    config1: u64,
    config2: u64,
}

impl CounterEventReport {
    pub(crate) fn new(
        counter_id: impl Into<String>,
        role: EventRole,
        config: u64,
        config1: u64,
        config2: u64,
    ) -> Self {
        Self {
            counter_id: counter_id.into(),
            role,
            config,
            config1,
            config2,
        }
    }

    pub fn counter_id(&self) -> &str {
        &self.counter_id
    }

    pub fn role(&self) -> EventRole {
        self.role
    }

    pub fn config(&self) -> u64 {
        self.config
    }

    pub fn config1(&self) -> u64 {
        self.config1
    }

    pub fn config2(&self) -> u64 {
        self.config2
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CounterRunReport {
    idle_baseline_duration: Duration,
    baseline_windows: Vec<CounterWindow>,
    sample_windows: Vec<CounterSample>,
    terminal_failure: Option<EvidenceFailure>,
}

impl CounterRunReport {
    pub(crate) fn new(
        idle_baseline_duration: Duration,
        baseline_windows: Vec<CounterWindow>,
        sample_windows: Vec<CounterSample>,
        terminal_failure: Option<EvidenceFailure>,
    ) -> Self {
        Self {
            idle_baseline_duration,
            baseline_windows,
            sample_windows,
            terminal_failure,
        }
    }

    pub fn idle_baseline_duration(&self) -> Duration {
        self.idle_baseline_duration
    }

    pub fn baseline_windows(&self) -> &[CounterWindow] {
        &self.baseline_windows
    }

    pub fn sample_windows(&self) -> &[CounterSample] {
        &self.sample_windows
    }

    pub fn terminal_failure(&self) -> Option<&EvidenceFailure> {
        self.terminal_failure.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CounterSample {
    zero_based_sample_index: u32,
    window: CounterWindow,
}

impl CounterSample {
    pub(crate) fn new(zero_based_sample_index: u32, window: CounterWindow) -> Self {
        Self {
            zero_based_sample_index,
            window,
        }
    }

    pub fn zero_based_sample_index(&self) -> u32 {
        self.zero_based_sample_index
    }

    pub fn window(&self) -> &CounterWindow {
        &self.window
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CounterWindow {
    before: CounterSnapshot,
    after: CounterSnapshot,
    elapsed: Duration,
    deltas: Vec<CounterDelta>,
    role_totals: Vec<RoleCounterTotal>,
}

impl CounterWindow {
    pub(crate) fn new(
        before: CounterSnapshot,
        after: CounterSnapshot,
        elapsed: Duration,
        deltas: Vec<CounterDelta>,
        role_totals: Vec<RoleCounterTotal>,
    ) -> Self {
        Self {
            before,
            after,
            elapsed,
            deltas,
            role_totals,
        }
    }

    pub fn before(&self) -> &CounterSnapshot {
        &self.before
    }

    pub fn after(&self) -> &CounterSnapshot {
        &self.after
    }

    pub fn elapsed(&self) -> Duration {
        self.elapsed
    }

    pub fn deltas(&self) -> &[CounterDelta] {
        &self.deltas
    }

    pub fn role_totals(&self) -> &[RoleCounterTotal] {
        &self.role_totals
    }

    pub(crate) fn role_total(&self, role: EventRole) -> Option<u64> {
        self.role_totals
            .iter()
            .find(|total| total.role == role)
            .map(|total| total.value)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RoleCounterTotal {
    role: EventRole,
    value: u64,
}

impl RoleCounterTotal {
    pub(crate) fn new(role: EventRole, value: u64) -> Self {
        Self { role, value }
    }

    pub fn role(self) -> EventRole {
        self.role
    }

    pub fn value(self) -> u64 {
        self.value
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum DiagnosticVerdict {
    CounterConsistentPeer {
        route_qualifier: Option<PeerRouteQualifier>,
    },
    CounterConsistentHostBounce,
    MixedSignalsAcrossRuns {
        route_qualifier: Option<PeerRouteQualifier>,
    },
    HeuristicOnly {
        likely: LikelyMechanism,
    },
    Indeterminate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PeerRouteQualifier {
    AcsRedirectedOrUpstreamRoutedPeerTrafficNotHostBounce,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LikelyMechanism {
    HostStaged,
    DeviceSide,
    LinkLimited,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvidenceReason {
    code: EvidenceReasonCode,
    message: String,
}

impl EvidenceReason {
    pub(crate) fn new(code: EvidenceReasonCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn code(&self) -> EvidenceReasonCode {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EvidenceReasonCode {
    CounterEvidenceUnavailable,
    CpuUnsupported,
    InsufficientCounterRepeats,
    CounterMultiplexed,
    CounterTerminalFailure,
    CounterSignalBelowGate,
    CounterOverflow,
    CounterTopologyMismatch,
    StagedCalibrationMissing,
    HeuristicHostStaged,
    HeuristicDeviceSide,
    HeuristicLinkLimited,
    HeuristicUnavailable,
    ThroughputSeparatedClusters,
    AcsStateChanged,
    AcsRedirectObserved,
}

pub(crate) fn link_theoretical_gb_s(link: &LinkInfo) -> Option<f64> {
    match link {
        LinkInfo::Known {
            theoretical_gb_s, ..
        } => Some(*theoretical_gb_s),
        LinkInfo::Unknown { .. } => None,
    }
}
