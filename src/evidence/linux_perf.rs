#![allow(
    unsafe_code,
    clippy::cast_possible_truncation,
    clippy::module_name_repetitions
)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io;
use std::mem::{offset_of, size_of};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::time::{Duration, Instant};

use super::counters::{CounterId, CounterReading, CounterSnapshot, CounterSource, EvidenceRunId};
use super::error::{EvidenceError, PerfEventFailure, PerfEventResourceFailure, Result};
use super::intel_perfmon::EventRole;
use super::linux_pmu::{PackedConfig, PmuInstance, PmuType};

const PERF_ATTR_SIZE_VER1: u32 = 72;
const PERF_FORMAT_TOTAL_TIME_ENABLED: u64 = 1 << 0;
const PERF_FORMAT_TOTAL_TIME_RUNNING: u64 = 1 << 1;
const PERF_READ_FORMAT: u64 = PERF_FORMAT_TOTAL_TIME_ENABLED | PERF_FORMAT_TOTAL_TIME_RUNNING;

const PERF_ATTR_FLAG_DISABLED: u64 = 1 << 0;
const PERF_ATTR_FLAG_PINNED: u64 = 1 << 2;
const PERF_FLAG_FD_CLOEXEC: libc::c_ulong = 1 << 3;

// Linux x86_64 _IO('$', nr) request values. xfer's uncore perf-event evidence
// currently targets Linux x86_64 Sapphire Rapids and Granite Rapids systems.
const PERF_EVENT_IOC_ENABLE: libc::c_ulong = 0x2400;
const PERF_EVENT_IOC_DISABLE: libc::c_ulong = 0x2401;
const PERF_EVENT_IOC_RESET: libc::c_ulong = 0x2403;
const PERF_IOC_FLAG_GROUP: libc::c_ulong = 1;

const PERF_READ_RECORD_SIZE: usize = 24;
const PERF_READ_BUFFER_SIZE: usize = PERF_READ_RECORD_SIZE + 8;

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PerfEventAttrV1 {
    type_: u32,
    size: u32,
    config: u64,
    sample_period: u64,
    sample_type: u64,
    read_format: u64,
    flags: u64,
    wakeup_events: u32,
    bp_type: u32,
    config1: u64,
    config2: u64,
}

const _: () = {
    assert!(size_of::<PerfEventAttrV1>() == PERF_ATTR_SIZE_VER1 as usize);
    assert!(offset_of!(PerfEventAttrV1, config2) == 64);
};

impl PerfEventAttrV1 {
    fn new(pmu_type: PmuType, config: PackedConfig, pinned_leader: bool) -> Self {
        let mut flags = PERF_ATTR_FLAG_DISABLED;
        if pinned_leader {
            flags |= PERF_ATTR_FLAG_PINNED;
        }

        Self {
            type_: pmu_type.value(),
            size: PERF_ATTR_SIZE_VER1,
            config: config.config,
            sample_period: 0,
            sample_type: 0,
            read_format: PERF_READ_FORMAT,
            flags,
            wakeup_events: 0,
            bp_type: 0,
            config1: config.config1,
            config2: config.config2,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LinuxPerfEventSpec {
    id: CounterId,
    role: EventRole,
    pmu_name: String,
    pmu_type: PmuType,
    cpu: i32,
    config: PackedConfig,
}

impl LinuxPerfEventSpec {
    pub fn new(
        id: CounterId,
        role: EventRole,
        pmu: &PmuInstance,
        config: PackedConfig,
    ) -> Result<Self> {
        let context = format!("PMU {} cpumask", pmu.name());
        let cpumask = pmu.cpumask().ok_or_else(|| {
            malformed_perf(
                context.clone(),
                "uncore PMU has no cpumask for system-wide perf_event_open",
            )
        })?;
        let cpu = parse_first_cpu(cpumask).map_err(|reason| malformed_perf(context, reason))?;
        Self::with_cpu(id, role, pmu.name(), pmu.pmu_type(), cpu, config)
    }

    pub fn with_cpu(
        id: CounterId,
        role: EventRole,
        pmu_name: impl Into<String>,
        pmu_type: PmuType,
        cpu: i32,
        config: PackedConfig,
    ) -> Result<Self> {
        if cpu < 0 {
            return Err(malformed_perf(
                "perf event CPU",
                format!("CPU {cpu} is negative"),
            ));
        }

        Ok(Self {
            id,
            role,
            pmu_name: pmu_name.into(),
            pmu_type,
            cpu,
            config,
        })
    }

    pub fn id(&self) -> CounterId {
        self.id
    }

    pub fn role(&self) -> EventRole {
        self.role
    }

    pub fn pmu_name(&self) -> &str {
        &self.pmu_name
    }

    pub fn pmu_type(&self) -> PmuType {
        self.pmu_type
    }

    pub fn cpu(&self) -> i32 {
        self.cpu
    }

    pub fn config(&self) -> PackedConfig {
        self.config
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LinuxPerfGroupSpec {
    pmu_name: String,
    pmu_type: PmuType,
    cpu: i32,
    events: Vec<LinuxPerfEventSpec>,
}

impl LinuxPerfGroupSpec {
    pub fn new(events: Vec<LinuxPerfEventSpec>) -> Result<Self> {
        let Some(first) = events.first() else {
            return Err(malformed_perf(
                "perf event group",
                "group must contain at least one event",
            ));
        };

        for event in &events {
            if event.pmu_name != first.pmu_name
                || event.pmu_type != first.pmu_type
                || event.cpu != first.cpu
            {
                return Err(malformed_perf(
                    "perf event group",
                    "all group members must use the same PMU instance and CPU",
                ));
            }
        }

        Ok(Self {
            pmu_name: first.pmu_name.clone(),
            pmu_type: first.pmu_type,
            cpu: first.cpu,
            events,
        })
    }

    pub fn pmu_name(&self) -> &str {
        &self.pmu_name
    }

    pub fn pmu_type(&self) -> PmuType {
        self.pmu_type
    }

    pub fn cpu(&self) -> i32 {
        self.cpu
    }

    pub fn events(&self) -> &[LinuxPerfEventSpec] {
        &self.events
    }
}

#[derive(Debug)]
pub struct LinuxPerfMeasurement {
    before: CounterSnapshot,
    after: CounterSnapshot,
}

impl LinuxPerfMeasurement {
    pub fn before(&self) -> &CounterSnapshot {
        &self.before
    }

    pub fn after(&self) -> &CounterSnapshot {
        &self.after
    }

    pub fn into_parts(self) -> (CounterSnapshot, CounterSnapshot) {
        (self.before, self.after)
    }
}

#[derive(Debug)]
pub struct LinuxPerfCounterSet {
    inner: PerfCounterSet<RealPerfEventOps>,
}

impl LinuxPerfCounterSet {
    pub fn open(run: EvidenceRunId, groups: Vec<LinuxPerfGroupSpec>) -> Result<Self> {
        PerfCounterSet::open_with_ops(run, groups, RealPerfEventOps).map(|inner| Self { inner })
    }

    pub fn begin_window(&mut self) -> Result<ActiveLinuxPerfWindow<'_>> {
        self.inner
            .begin_window()
            .map(|inner| ActiveLinuxPerfWindow { inner })
    }

    pub fn measure<T, E>(
        &mut self,
        operation: impl FnOnce() -> std::result::Result<T, E>,
    ) -> (std::result::Result<T, E>, Result<LinuxPerfMeasurement>) {
        self.inner.measure(operation)
    }
}

#[must_use = "dropping an active perf window disables its groups but discards readings"]
pub struct ActiveLinuxPerfWindow<'a> {
    inner: ActivePerfCounterSet<'a, RealPerfEventOps>,
}

impl ActiveLinuxPerfWindow<'_> {
    pub fn finish(self) -> Result<(CounterSnapshot, CounterSnapshot)> {
        self.inner.finish()
    }
}

impl CounterSource for LinuxPerfCounterSet {
    fn snapshot(&mut self) -> Result<CounterSnapshot> {
        self.inner.snapshot()
    }
}

#[derive(Debug)]
struct PerfCounterSet<O> {
    run: EvidenceRunId,
    groups: Vec<OpenPerfGroup>,
    ops: O,
    state: PerfCounterSetState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PerfCounterSetState {
    Ready,
    ClosedAfterEnableCleanupFailure,
}

impl<O: PerfEventOps> PerfCounterSet<O> {
    fn open_with_ops(
        run: EvidenceRunId,
        groups: Vec<LinuxPerfGroupSpec>,
        mut ops: O,
    ) -> Result<Self> {
        if groups.is_empty() {
            return Err(malformed_perf(
                "perf counter set",
                "counter set must contain at least one group",
            ));
        }
        validate_unique_event_ids(&groups)?;

        let mut open_groups = Vec::with_capacity(groups.len());
        for group in groups {
            open_groups.push(open_group(group, &mut ops)?);
        }

        Ok(Self {
            run,
            groups: open_groups,
            ops,
            state: PerfCounterSetState::Ready,
        })
    }

    fn begin_window(&mut self) -> Result<ActivePerfCounterSet<'_, O>> {
        self.ensure_ready("begin perf-event window")?;
        self.reset_all()?;
        self.enable_all()?;

        let mut guard = ActivePerfCounterSet::new(self);
        let before = guard.set.snapshot()?;
        guard.before = Some(before);
        Ok(guard)
    }

    fn measure<T, E>(
        &mut self,
        operation: impl FnOnce() -> std::result::Result<T, E>,
    ) -> (std::result::Result<T, E>, Result<LinuxPerfMeasurement>) {
        match self.begin_window() {
            Ok(window) => {
                let operation_result = operation();
                let evidence_result = window
                    .finish()
                    .map(|(before, after)| LinuxPerfMeasurement { before, after });
                (operation_result, evidence_result)
            }
            Err(error) => (operation(), Err(error)),
        }
    }

    fn snapshot(&mut self) -> Result<CounterSnapshot> {
        self.ensure_ready("read perf-event counter set")?;
        self.snapshot_at(Instant::now())
    }

    fn ensure_ready(&self, context: &str) -> Result<()> {
        if self.state == PerfCounterSetState::ClosedAfterEnableCleanupFailure {
            return Err(closed_perf(
                context,
                "group fds were closed after an enable cleanup failure",
            ));
        }
        Ok(())
    }

    fn snapshot_at(&mut self, taken_at: Instant) -> Result<CounterSnapshot> {
        let mut readings = Vec::new();
        for group in &self.groups {
            for event in &group.events {
                let context = format!("read {} {}", event.spec.pmu_name, event.spec.role);
                let record = self.ops.read_event(&event.fd, &context)?;
                readings.push(CounterReading::new(
                    event.spec.id,
                    event.spec.role,
                    record.value,
                    Duration::from_nanos(record.time_enabled),
                    Duration::from_nanos(record.time_running),
                )?);
            }
        }
        CounterSnapshot::new(self.run, taken_at, readings)
    }

    fn reset_all(&mut self) -> Result<()> {
        for group in &self.groups {
            let context = group.control_context("reset");
            self.ops
                .ioctl_group(&group.leader().fd, PERF_EVENT_IOC_RESET, &context)?;
        }
        Ok(())
    }

    fn enable_all(&mut self) -> Result<()> {
        let mut enabled = Vec::new();
        for index in 0..self.groups.len() {
            let group = &self.groups[index];
            let context = group.control_context("enable");
            match self
                .ops
                .ioctl_group(&group.leader().fd, PERF_EVENT_IOC_ENABLE, &context)
            {
                Ok(()) => enabled.push(group.leader().fd.as_raw_fd()),
                Err(error) => {
                    let cleanup_failures = self.disable_leaders(enabled);
                    if cleanup_failures.is_empty() {
                        return Err(error);
                    }

                    let mut failures = vec![error];
                    failures.extend(cleanup_failures);
                    self.state = PerfCounterSetState::ClosedAfterEnableCleanupFailure;
                    self.groups.clear();
                    return Err(combine_failures("enable perf-event groups", failures));
                }
            }
        }
        Ok(())
    }

    fn disable_all(&mut self) -> Result<()> {
        let leaders = self
            .groups
            .iter()
            .map(|group| group.leader().fd.as_raw_fd())
            .collect::<Vec<_>>();
        let failures = self.disable_leaders(leaders);
        if failures.is_empty() {
            Ok(())
        } else {
            Err(combine_failures("disable perf-event groups", failures))
        }
    }

    fn disable_leaders(&mut self, leaders: Vec<RawFd>) -> Vec<EvidenceError> {
        let mut failures = Vec::new();
        for leader in leaders {
            let Some(group) = self
                .groups
                .iter()
                .find(|group| group.leader().fd.as_raw_fd() == leader)
            else {
                failures.push(malformed_perf(
                    "disable perf-event group",
                    format!("leader fd {leader} is not part of the counter set"),
                ));
                continue;
            };
            let context = group.control_context("disable");
            if let Err(error) =
                self.ops
                    .ioctl_group(&group.leader().fd, PERF_EVENT_IOC_DISABLE, &context)
            {
                failures.push(error);
            }
        }
        failures
    }
}

struct ActivePerfCounterSet<'a, O: PerfEventOps> {
    set: &'a mut PerfCounterSet<O>,
    before: Option<CounterSnapshot>,
    active: bool,
}

impl<'a, O: PerfEventOps> ActivePerfCounterSet<'a, O> {
    fn new(set: &'a mut PerfCounterSet<O>) -> Self {
        Self {
            set,
            before: None,
            active: true,
        }
    }

    fn finish(mut self) -> Result<(CounterSnapshot, CounterSnapshot)> {
        let disable_result = self.disable();
        let after_result = self.set.snapshot();
        let before = self.before.take().ok_or_else(|| {
            malformed_perf(
                "finish perf-event window",
                "active window has no initial snapshot",
            )
        })?;

        match (disable_result, after_result) {
            (Ok(()), Ok(after)) => Ok((before, after)),
            (disable_result, after_result) => {
                let mut failures = Vec::new();
                if let Err(error) = disable_result {
                    failures.push(error);
                }
                if let Err(error) = after_result {
                    failures.push(error);
                }
                Err(combine_failures("finish perf-event window", failures))
            }
        }
    }

    fn disable(&mut self) -> Result<()> {
        let result = self.set.disable_all();
        if result.is_ok() {
            self.active = false;
        }
        result
    }
}

impl<O: PerfEventOps> Drop for ActivePerfCounterSet<'_, O> {
    fn drop(&mut self) {
        if self.active {
            let _ = self.set.disable_all();
            self.active = false;
        }
    }
}

#[derive(Debug)]
struct OpenPerfGroup {
    pmu_name: String,
    cpu: i32,
    events: Vec<OpenPerfEvent>,
}

impl OpenPerfGroup {
    fn leader(&self) -> &OpenPerfEvent {
        self.events
            .first()
            .expect("open perf groups are constructed from non-empty specs")
    }

    fn control_context(&self, action: &str) -> String {
        format!("{action} PMU {} group on CPU {}", self.pmu_name, self.cpu)
    }
}

#[derive(Debug)]
struct OpenPerfEvent {
    spec: LinuxPerfEventSpec,
    fd: OwnedFd,
}

fn open_group<O: PerfEventOps>(group: LinuxPerfGroupSpec, ops: &mut O) -> Result<OpenPerfGroup> {
    let mut events = Vec::with_capacity(group.events.len());
    for spec in group.events {
        let is_leader = events.is_empty();
        let group_fd = events
            .first()
            .map_or(-1, |event: &OpenPerfEvent| event.fd.as_raw_fd());
        let attr = PerfEventAttrV1::new(spec.pmu_type, spec.config, is_leader);
        let context = if is_leader {
            format!(
                "{} {} on CPU {} as pinned group leader",
                spec.pmu_name, spec.role, spec.cpu
            )
        } else {
            format!(
                "{} {} on CPU {} as group member",
                spec.pmu_name, spec.role, spec.cpu
            )
        };
        let fd = ops.open_event(&attr, spec.cpu, group_fd, &context)?;
        events.push(OpenPerfEvent { spec, fd });
    }

    Ok(OpenPerfGroup {
        pmu_name: group.pmu_name,
        cpu: group.cpu,
        events,
    })
}

trait PerfEventOps {
    fn open_event(
        &mut self,
        attr: &PerfEventAttrV1,
        cpu: i32,
        group_fd: RawFd,
        context: &str,
    ) -> Result<OwnedFd>;

    fn ioctl_group(&mut self, fd: &OwnedFd, request: libc::c_ulong, context: &str) -> Result<()>;

    fn read_event(&mut self, fd: &OwnedFd, context: &str) -> Result<PerfReadRecord>;
}

#[derive(Debug)]
struct RealPerfEventOps;

impl PerfEventOps for RealPerfEventOps {
    fn open_event(
        &mut self,
        attr: &PerfEventAttrV1,
        cpu: i32,
        group_fd: RawFd,
        context: &str,
    ) -> Result<OwnedFd> {
        loop {
            // SAFETY: The pointer references a live repr(C) perf_event_attr
            // prefix whose size field is set to PERF_ATTR_SIZE_VER1. pid=-1
            // and cpu is a selected non-negative PMU cpumask CPU for a
            // system-wide uncore event. The raw fd returned by the kernel is
            // immediately wrapped in OwnedFd on success.
            let result = unsafe {
                libc::syscall(
                    libc::SYS_perf_event_open,
                    std::ptr::from_ref(attr),
                    -1_i32,
                    cpu,
                    group_fd,
                    PERF_FLAG_FD_CLOEXEC,
                )
            };
            if result >= 0 {
                let raw_fd = i32::try_from(result).map_err(|error| {
                    malformed_perf(
                        context,
                        format!("kernel returned fd outside RawFd range: {error}"),
                    )
                })?;
                // SAFETY: perf_event_open returned this fd successfully and
                // ownership has not been transferred elsewhere.
                return Ok(unsafe { OwnedFd::from_raw_fd(raw_fd) });
            }

            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(map_perf_errno(PerfOperation::Open, context, error));
        }
    }

    fn ioctl_group(&mut self, fd: &OwnedFd, request: libc::c_ulong, context: &str) -> Result<()> {
        loop {
            // SAFETY: fd is owned and valid for the duration of the call.
            // request is one of the audited perf_event ioctl group-control
            // constants above and the variadic argument is PERF_IOC_FLAG_GROUP.
            let result = unsafe { libc::ioctl(fd.as_raw_fd(), request, PERF_IOC_FLAG_GROUP) };
            if result == 0 {
                return Ok(());
            }

            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(map_perf_errno(PerfOperation::Ioctl, context, error));
        }
    }

    fn read_event(&mut self, fd: &OwnedFd, context: &str) -> Result<PerfReadRecord> {
        read_perf_record(fd, context)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PerfReadRecord {
    value: u64,
    time_enabled: u64,
    time_running: u64,
}

fn read_perf_record(fd: &OwnedFd, context: &str) -> Result<PerfReadRecord> {
    let mut buffer = [0_u8; PERF_READ_BUFFER_SIZE];
    loop {
        // SAFETY: buffer is valid for writes of buffer.len() bytes, and fd is
        // an owned file descriptor that remains open for the syscall.
        let result = unsafe {
            libc::read(
                fd.as_raw_fd(),
                buffer.as_mut_ptr().cast::<libc::c_void>(),
                buffer.len(),
            )
        };
        if result >= 0 {
            let len = usize::try_from(result).map_err(|error| {
                malformed_perf(
                    context,
                    format!("read returned byte count outside usize range: {error}"),
                )
            })?;
            return decode_perf_read(&buffer[..len], context);
        }

        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        return Err(map_perf_errno(PerfOperation::Read, context, error));
    }
}

fn decode_perf_read(bytes: &[u8], context: &str) -> Result<PerfReadRecord> {
    if bytes.len() < PERF_READ_RECORD_SIZE {
        return Err(malformed_perf(
            context,
            format!(
                "short read: got {} bytes, expected {PERF_READ_RECORD_SIZE}",
                bytes.len()
            ),
        ));
    }
    if bytes.len() > PERF_READ_RECORD_SIZE {
        return Err(malformed_perf(
            context,
            format!(
                "extra read payload: got {} bytes, expected {PERF_READ_RECORD_SIZE}",
                bytes.len()
            ),
        ));
    }

    let value = read_u64_ne(&bytes[0..8]);
    let time_enabled = read_u64_ne(&bytes[8..16]);
    let time_running = read_u64_ne(&bytes[16..24]);
    if time_running > time_enabled {
        return Err(malformed_perf(
            context,
            format!("time_running {time_running} exceeds time_enabled {time_enabled}"),
        ));
    }

    Ok(PerfReadRecord {
        value,
        time_enabled,
        time_running,
    })
}

fn read_u64_ne(bytes: &[u8]) -> u64 {
    let mut array = [0_u8; 8];
    array.copy_from_slice(bytes);
    u64::from_ne_bytes(array)
}

pub fn parse_first_cpu(cpulist: &str) -> std::result::Result<i32, String> {
    let cpulist = cpulist.trim();
    if cpulist.is_empty() {
        return Err("cpulist is empty".to_owned());
    }

    let mut first_cpu = None;
    let mut ranges = BTreeMap::<i32, i32>::new();
    for segment in cpulist.split(',') {
        if segment.is_empty() {
            return Err("cpulist contains an empty segment".to_owned());
        }
        if segment.as_bytes().iter().any(u8::is_ascii_whitespace) {
            return Err(format!(
                "cpulist segment '{segment}' contains inner whitespace"
            ));
        }

        let (start, end) = if let Some((start, end)) = segment.split_once('-') {
            if end.contains('-') {
                return Err(format!(
                    "cpulist segment '{segment}' has too many '-' separators"
                ));
            }
            let start = parse_cpu_number(start)?;
            let end = parse_cpu_number(end)?;
            if start > end {
                return Err(format!("cpulist range '{segment}' is reversed"));
            }
            (start, end)
        } else {
            let cpu = parse_cpu_number(segment)?;
            (cpu, cpu)
        };

        if first_cpu.is_none() {
            first_cpu = Some(start);
        }

        if let Some((&seen_start, &seen_end)) = ranges.range(..=start).next_back() {
            if seen_end >= start {
                let overlap = start.max(seen_start);
                return Err(format!(
                    "cpulist contains duplicate or overlapping CPU {overlap}"
                ));
            }
        }
        if let Some((&seen_start, _)) = ranges.range(start..).next() {
            if seen_start <= end {
                return Err(format!(
                    "cpulist contains duplicate or overlapping CPU {seen_start}"
                ));
            }
        }
        ranges.insert(start, end);
    }

    first_cpu.ok_or_else(|| "cpulist is empty".to_owned())
}

fn parse_cpu_number(text: &str) -> std::result::Result<i32, String> {
    if text.is_empty() {
        return Err("CPU number is empty".to_owned());
    }
    if !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!(
            "CPU number '{text}' is not an unsigned decimal integer"
        ));
    }
    if text.len() > 1 && text.starts_with('0') {
        return Err(format!("CPU number '{text}' is not canonical"));
    }

    let value = text
        .parse::<u64>()
        .map_err(|error| format!("CPU number '{text}' is malformed: {error}"))?;
    let max = i32::MAX as u64;
    if value > max {
        return Err(format!("CPU number '{text}' exceeds i32::MAX"));
    }
    i32::try_from(value).map_err(|error| format!("CPU number '{text}' is invalid: {error}"))
}

fn validate_unique_event_ids(groups: &[LinuxPerfGroupSpec]) -> Result<()> {
    let mut ids = BTreeSet::new();
    for group in groups {
        for event in group.events() {
            if !ids.insert(event.id()) {
                return Err(malformed_perf(
                    "perf counter set",
                    format!("duplicate counter id {}", event.id()),
                ));
            }
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PerfOperation {
    Open,
    Ioctl,
    Read,
}

impl fmt::Display for PerfOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Open => f.write_str("perf_event_open"),
            Self::Ioctl => f.write_str("perf_event ioctl"),
            Self::Read => f.write_str("perf_event read"),
        }
    }
}

fn map_perf_errno(operation: PerfOperation, context: &str, error: io::Error) -> EvidenceError {
    let errno = error.raw_os_error();
    let context = format!("{operation} {context}");
    let failure = match errno {
        Some(libc::EACCES | libc::EPERM) => PerfEventFailure::PermissionDenied {
            context,
            source: error,
        },
        Some(libc::ENOENT | libc::ENODEV | libc::EOPNOTSUPP | libc::ENOSYS) => {
            PerfEventFailure::Unsupported {
                context,
                source: error,
            }
        }
        Some(libc::EMFILE | libc::ENFILE | libc::ENOMEM | libc::EBUSY) => {
            PerfEventFailure::ResourceUnavailable {
                context,
                source: PerfEventResourceFailure::System(error),
            }
        }
        Some(libc::EINVAL) if operation == PerfOperation::Open => PerfEventFailure::RejectedAttr {
            context,
            source: error,
        },
        _ => PerfEventFailure::Io {
            context,
            source: error,
        },
    };
    EvidenceError::PerfEvent(failure)
}

fn malformed_perf(context: impl Into<String>, reason: impl Into<String>) -> EvidenceError {
    EvidenceError::PerfEvent(PerfEventFailure::Malformed {
        context: context.into(),
        reason: reason.into(),
    })
}

fn closed_perf(context: impl Into<String>, reason: impl Into<String>) -> EvidenceError {
    EvidenceError::PerfEvent(PerfEventFailure::ResourceUnavailable {
        context: context.into(),
        source: PerfEventResourceFailure::Closed {
            reason: reason.into(),
        },
    })
}

fn combine_failures(context: impl Into<String>, failures: Vec<EvidenceError>) -> EvidenceError {
    let mut failures = failures.into_iter();
    let Some(first) = failures.next() else {
        return malformed_perf(context, "no failures were provided");
    };
    let Some(second) = failures.next() else {
        return first;
    };

    let mut combined = vec![first, second];
    combined.extend(failures);
    EvidenceError::PerfEvent(PerfEventFailure::Multiple {
        context: context.into(),
        failures: combined,
    })
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::{BTreeSet, VecDeque};
    use std::error::Error as _;
    use std::fs::File;
    use std::io::Write;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use super::super::counters::diff_snapshots;
    use super::*;

    #[test]
    fn perf_attr_layout_matches_linux_v1_prefix() {
        assert_eq!(size_of::<PerfEventAttrV1>(), 72);
        assert_eq!(offset_of!(PerfEventAttrV1, type_), 0);
        assert_eq!(offset_of!(PerfEventAttrV1, config), 8);
        assert_eq!(offset_of!(PerfEventAttrV1, flags), 40);
        assert_eq!(offset_of!(PerfEventAttrV1, wakeup_events), 48);
        assert_eq!(offset_of!(PerfEventAttrV1, config1), 56);
        assert_eq!(offset_of!(PerfEventAttrV1, config2), 64);
    }

    #[test]
    fn perf_attr_sets_minimal_uncore_fields() {
        let attr = PerfEventAttrV1::new(
            PmuType::new(67),
            PackedConfig {
                config: 1,
                config1: 2,
                config2: 3,
            },
            true,
        );

        assert_eq!(attr.type_, 67);
        assert_eq!(attr.size, PERF_ATTR_SIZE_VER1);
        assert_eq!(attr.config, 1);
        assert_eq!(attr.config1, 2);
        assert_eq!(attr.config2, 3);
        assert_eq!(attr.sample_period, 0);
        assert_eq!(attr.sample_type, 0);
        assert_eq!(attr.read_format, PERF_READ_FORMAT);
        assert_eq!(attr.flags, PERF_ATTR_FLAG_DISABLED | PERF_ATTR_FLAG_PINNED);
    }

    #[test]
    fn parses_strict_cpulists_and_chooses_first_cpu() {
        assert_eq!(parse_first_cpu("0").expect("cpu"), 0);
        assert_eq!(parse_first_cpu("0-3").expect("cpu"), 0);
        assert_eq!(parse_first_cpu("0,4-7").expect("cpu"), 0);
        assert_eq!(parse_first_cpu(" 4-7,0 ").expect("cpu"), 4);
    }

    #[test]
    fn parses_huge_valid_range_without_expanding_it() {
        assert_eq!(
            parse_first_cpu("0-2147483647").expect("full i32 CPU range"),
            0
        );
    }

    #[test]
    fn rejects_noncanonical_cpulists() {
        for text in [
            "",
            " ",
            ",0",
            "0,",
            "0,,1",
            "+1",
            "-1",
            "01",
            "0-",
            "3-0",
            "0-3,2",
            "4-7,0-5",
            "0,0",
            "0, 1",
            "0-3-5",
            "2147483648",
            "abc",
        ] {
            assert!(parse_first_cpu(text).is_err(), "{text:?} should fail");
        }
    }

    #[test]
    fn decodes_exact_perf_read_payload() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&123_u64.to_ne_bytes());
        bytes.extend_from_slice(&456_u64.to_ne_bytes());
        bytes.extend_from_slice(&456_u64.to_ne_bytes());

        let record = decode_perf_read(&bytes, "test").expect("record");

        assert_eq!(
            record,
            PerfReadRecord {
                value: 123,
                time_enabled: 456,
                time_running: 456,
            }
        );
    }

    #[test]
    fn rejects_short_extra_and_malformed_timing_reads() {
        assert!(matches!(
            decode_perf_read(&[0; 23], "short"),
            Err(EvidenceError::PerfEvent(PerfEventFailure::Malformed { .. }))
        ));
        assert!(matches!(
            decode_perf_read(&[0; 25], "extra"),
            Err(EvidenceError::PerfEvent(PerfEventFailure::Malformed { .. }))
        ));

        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1_u64.to_ne_bytes());
        bytes.extend_from_slice(&10_u64.to_ne_bytes());
        bytes.extend_from_slice(&11_u64.to_ne_bytes());
        assert!(matches!(
            decode_perf_read(&bytes, "timing"),
            Err(EvidenceError::PerfEvent(PerfEventFailure::Malformed { .. }))
        ));
    }

    #[test]
    fn reads_exact_payloads_from_pipe_and_rejects_short_reads() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&5_u64.to_ne_bytes());
        bytes.extend_from_slice(&7_u64.to_ne_bytes());
        bytes.extend_from_slice(&7_u64.to_ne_bytes());
        let read_fd = pipe_with_bytes(&bytes);
        assert_eq!(
            read_perf_record(&read_fd, "pipe").expect("record"),
            PerfReadRecord {
                value: 5,
                time_enabled: 7,
                time_running: 7,
            }
        );

        let short_fd = pipe_with_bytes(&bytes[..16]);
        assert!(matches!(
            read_perf_record(&short_fd, "short pipe"),
            Err(EvidenceError::PerfEvent(PerfEventFailure::Malformed { .. }))
        ));
    }

    #[test]
    fn maps_perf_errno_to_typed_outcomes() {
        assert!(matches!(
            map_perf_errno(
                PerfOperation::Open,
                "ctx",
                io::Error::from_raw_os_error(libc::EACCES)
            ),
            EvidenceError::PerfEvent(PerfEventFailure::PermissionDenied { .. })
        ));
        assert!(matches!(
            map_perf_errno(
                PerfOperation::Open,
                "ctx",
                io::Error::from_raw_os_error(libc::ENODEV)
            ),
            EvidenceError::PerfEvent(PerfEventFailure::Unsupported { .. })
        ));
        assert!(matches!(
            map_perf_errno(
                PerfOperation::Open,
                "ctx",
                io::Error::from_raw_os_error(libc::ENOMEM)
            ),
            EvidenceError::PerfEvent(PerfEventFailure::ResourceUnavailable { .. })
        ));
        assert!(matches!(
            map_perf_errno(
                PerfOperation::Open,
                "ctx",
                io::Error::from_raw_os_error(libc::EINVAL)
            ),
            EvidenceError::PerfEvent(PerfEventFailure::RejectedAttr { .. })
        ));
        assert!(matches!(
            map_perf_errno(
                PerfOperation::Read,
                "ctx",
                io::Error::from_raw_os_error(libc::EINVAL)
            ),
            EvidenceError::PerfEvent(PerfEventFailure::Io { .. })
        ));
    }

    #[test]
    fn combined_failures_preserve_every_typed_error() {
        let error = combine_failures(
            "typed failures",
            vec![
                map_perf_errno(
                    PerfOperation::Open,
                    "permission",
                    io::Error::from_raw_os_error(libc::EACCES),
                ),
                map_perf_errno(
                    PerfOperation::Open,
                    "unsupported",
                    io::Error::from_raw_os_error(libc::ENODEV),
                ),
                map_perf_errno(
                    PerfOperation::Open,
                    "resource",
                    io::Error::from_raw_os_error(libc::ENOMEM),
                ),
                map_perf_errno(
                    PerfOperation::Open,
                    "rejected",
                    io::Error::from_raw_os_error(libc::EINVAL),
                ),
                malformed_perf("malformed", "bad payload"),
                map_perf_errno(
                    PerfOperation::Read,
                    "io",
                    io::Error::from_raw_os_error(libc::EIO),
                ),
            ],
        );

        let EvidenceError::PerfEvent(PerfEventFailure::Multiple { failures, .. }) = &error else {
            panic!("unexpected error: {error:?}");
        };
        assert!(matches!(
            failures.as_slice(),
            [
                EvidenceError::PerfEvent(PerfEventFailure::PermissionDenied { .. }),
                EvidenceError::PerfEvent(PerfEventFailure::Unsupported { .. }),
                EvidenceError::PerfEvent(PerfEventFailure::ResourceUnavailable {
                    source: PerfEventResourceFailure::System(_),
                    ..
                }),
                EvidenceError::PerfEvent(PerfEventFailure::RejectedAttr { .. }),
                EvidenceError::PerfEvent(PerfEventFailure::Malformed { .. }),
                EvidenceError::PerfEvent(PerfEventFailure::Io { .. }),
            ]
        ));
        assert!(error.source().is_some());
    }

    #[test]
    fn window_resets_enables_disables_and_reads_in_order() {
        let ops = FakeOps::new();
        let mut set = PerfCounterSet::open_with_ops(
            EvidenceRunId::new(1),
            vec![group(vec![event(0), event(1)])],
            ops,
        )
        .expect("open");
        let copied = Cell::new(false);

        let window = set.begin_window().expect("begin window");
        copied.set(true);
        let (before, after) = window.finish().expect("finish window");

        assert!(copied.get());
        assert_eq!(before.readings()[0].value(), 100);
        assert_eq!(before.readings()[1].value(), 101);
        assert_eq!(after.readings()[0].value(), 102);
        assert_eq!(after.readings()[1].value(), 103);
        assert!(after.taken_at() >= before.taken_at());
        assert_eq!(
            set.ops.calls,
            vec![
                "open:-1",
                "open:leader",
                "ioctl:reset",
                "ioctl:enable",
                "read",
                "read",
                "ioctl:disable",
                "read",
                "read",
            ]
        );
    }

    #[test]
    fn snapshot_delta_excludes_pre_window_counts_and_times() {
        let ops = FakeOps::with_reads([
            PerfReadRecord {
                value: 1_000,
                time_enabled: 50,
                time_running: 40,
            },
            PerfReadRecord {
                value: 1_017,
                time_enabled: 59,
                time_running: 49,
            },
        ]);
        let mut set =
            PerfCounterSet::open_with_ops(EvidenceRunId::new(1), vec![group(vec![event(0)])], ops)
                .expect("open");

        let (before, after) = set
            .begin_window()
            .expect("begin window")
            .finish()
            .expect("finish window");
        let deltas = diff_snapshots(&before, &after).expect("snapshot delta");
        let delta = &deltas.deltas()[0];

        assert_eq!(delta.value(), 17);
        assert_eq!(delta.time_enabled(), Duration::from_nanos(9));
        assert_eq!(delta.time_running(), Duration::from_nanos(9));
    }

    #[test]
    fn operation_errors_remain_distinct_from_evidence_errors() {
        #[derive(Debug, Eq, PartialEq)]
        struct OperationError;

        let mut ops = FakeOps::new();
        ops.fail_read_at = Some(2);
        let mut set =
            PerfCounterSet::open_with_ops(EvidenceRunId::new(1), vec![group(vec![event(0)])], ops)
                .expect("open");

        let (operation_result, evidence_result) =
            set.measure(|| Err::<(), OperationError>(OperationError));

        assert_eq!(operation_result, Err(OperationError));
        assert!(matches!(
            evidence_result,
            Err(EvidenceError::PerfEvent(PerfEventFailure::Malformed { .. }))
        ));
    }

    #[test]
    fn measure_runs_operation_once_when_evidence_setup_fails() {
        #[derive(Debug, Eq, PartialEq)]
        struct OperationError;

        let mut ops = FakeOps::new();
        ops.fail_enable_at = Some(1);
        let mut set =
            PerfCounterSet::open_with_ops(EvidenceRunId::new(1), vec![group(vec![event(0)])], ops)
                .expect("open");
        let calls = Cell::new(0);

        let (operation_result, evidence_result) = set.measure(|| {
            calls.set(calls.get() + 1);
            Err::<(), OperationError>(OperationError)
        });

        assert_eq!(calls.get(), 1);
        assert_eq!(operation_result, Err(OperationError));
        assert!(matches!(
            evidence_result,
            Err(EvidenceError::PerfEvent(
                PerfEventFailure::ResourceUnavailable { .. }
            ))
        ));
        assert_eq!(set.state, PerfCounterSetState::Ready);
        assert_eq!(set.groups.len(), 1);
    }

    #[test]
    fn failed_before_snapshot_disables_through_guard_drop() {
        let mut ops = FakeOps::new();
        ops.fail_read_at = Some(1);
        let mut set =
            PerfCounterSet::open_with_ops(EvidenceRunId::new(1), vec![group(vec![event(0)])], ops)
                .expect("open");

        let error = set
            .begin_window()
            .err()
            .expect("before snapshot should fail");

        assert!(matches!(
            error,
            EvidenceError::PerfEvent(PerfEventFailure::Malformed { .. })
        ));
        assert!(set.ops.enabled.is_empty());
        assert_eq!(
            set.ops.calls,
            vec![
                "open:-1",
                "ioctl:reset",
                "ioctl:enable",
                "read",
                "ioctl:disable",
            ]
        );
    }

    #[test]
    fn dropping_window_during_unwind_disables_groups() {
        let ops = FakeOps::new();
        let mut set =
            PerfCounterSet::open_with_ops(EvidenceRunId::new(1), vec![group(vec![event(0)])], ops)
                .expect("open");

        let unwind = catch_unwind(AssertUnwindSafe(|| {
            let _window = set.begin_window().expect("begin window");
            panic!("synthetic operation panic");
        }));

        assert!(unwind.is_err());
        assert!(set.ops.enabled.is_empty());
        assert_eq!(
            set.ops.calls,
            vec![
                "open:-1",
                "ioctl:reset",
                "ioctl:enable",
                "read",
                "ioctl:disable",
            ]
        );
    }

    #[test]
    fn finish_retains_disable_and_read_failures() {
        let mut ops = FakeOps::new();
        ops.fail_disable = true;
        ops.fail_read_at = Some(2);
        let mut set =
            PerfCounterSet::open_with_ops(EvidenceRunId::new(1), vec![group(vec![event(0)])], ops)
                .expect("open");

        let error = set
            .begin_window()
            .expect("begin window")
            .finish()
            .expect_err("multiple errors");

        match error {
            EvidenceError::PerfEvent(PerfEventFailure::Multiple { failures, .. }) => {
                assert_eq!(failures.len(), 2);
                assert!(matches!(
                    failures.as_slice(),
                    [
                        EvidenceError::PerfEvent(PerfEventFailure::ResourceUnavailable { .. }),
                        EvidenceError::PerfEvent(PerfEventFailure::Malformed { .. }),
                    ]
                ));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn enable_failure_with_successful_cleanup_keeps_set_retryable() {
        let mut ops = FakeOps::new();
        ops.fail_enable_at = Some(2);
        let mut set = PerfCounterSet::open_with_ops(
            EvidenceRunId::new(1),
            vec![group(vec![event(0)]), group(vec![event(1)])],
            ops,
        )
        .expect("open");

        assert!(set.begin_window().is_err());
        assert!(set.ops.enabled.is_empty());
        assert_eq!(set.state, PerfCounterSetState::Ready);
        assert_eq!(set.groups.len(), 2);
        assert_eq!(
            set.ops.calls,
            vec![
                "open:-1",
                "open:-1",
                "ioctl:reset",
                "ioctl:reset",
                "ioctl:enable",
                "ioctl:enable",
                "ioctl:disable",
            ]
        );

        set.begin_window()
            .expect("retry begin")
            .finish()
            .expect("retry finish");
    }

    #[test]
    fn enable_cleanup_failure_closes_set_with_typed_terminal_state() {
        let mut ops = FakeOps::new();
        ops.fail_enable_at = Some(2);
        ops.fail_disable = true;
        let mut set = PerfCounterSet::open_with_ops(
            EvidenceRunId::new(1),
            vec![group(vec![event(0)]), group(vec![event(1)])],
            ops,
        )
        .expect("open");

        let first = set.begin_window().err().expect("enable should fail");
        assert!(matches!(
            first,
            EvidenceError::PerfEvent(PerfEventFailure::Multiple { .. })
        ));
        assert_eq!(
            set.state,
            PerfCounterSetState::ClosedAfterEnableCleanupFailure
        );
        assert!(set.groups.is_empty());

        let second = set.begin_window().err().expect("closed set should fail");
        assert!(matches!(
            second,
            EvidenceError::PerfEvent(PerfEventFailure::ResourceUnavailable {
                source: PerfEventResourceFailure::Closed { .. },
                ..
            })
        ));
        assert!(matches!(
            set.snapshot(),
            Err(EvidenceError::PerfEvent(
                PerfEventFailure::ResourceUnavailable {
                    source: PerfEventResourceFailure::Closed { .. },
                    ..
                }
            ))
        ));
    }

    fn event(id: u32) -> LinuxPerfEventSpec {
        LinuxPerfEventSpec::with_cpu(
            CounterId::new(id),
            EventRole::IioDataReqOfCpuPeerWriteAllParts,
            "uncore_iio_0",
            PmuType::new(67),
            0,
            PackedConfig::default(),
        )
        .expect("event")
    }

    fn group(events: Vec<LinuxPerfEventSpec>) -> LinuxPerfGroupSpec {
        LinuxPerfGroupSpec::new(events).expect("group")
    }

    fn pipe_with_bytes(bytes: &[u8]) -> OwnedFd {
        let mut fds = [0_i32; 2];
        // SAFETY: fds points to two valid i32 slots for libc::pipe to fill.
        let result = unsafe { libc::pipe(fds.as_mut_ptr()) };
        assert_eq!(result, 0, "pipe failed: {}", io::Error::last_os_error());
        // SAFETY: pipe initialized both file descriptors and ownership is
        // transferred into OwnedFd exactly once.
        let read_fd = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        // SAFETY: pipe initialized both file descriptors and ownership is
        // transferred into OwnedFd exactly once.
        let write_fd = unsafe { OwnedFd::from_raw_fd(fds[1]) };
        let mut writer = File::from(write_fd);
        writer.write_all(bytes).expect("write pipe");
        drop(writer);
        read_fd
    }

    #[derive(Debug)]
    struct FakeOps {
        calls: Vec<&'static str>,
        next_read: u64,
        fail_disable: bool,
        fail_read_at: Option<usize>,
        read_attempts: usize,
        fail_enable_at: Option<usize>,
        enable_attempts: usize,
        enabled: BTreeSet<RawFd>,
        scripted_reads: VecDeque<PerfReadRecord>,
    }

    impl FakeOps {
        fn new() -> Self {
            Self {
                calls: Vec::new(),
                next_read: 100,
                fail_disable: false,
                fail_read_at: None,
                read_attempts: 0,
                fail_enable_at: None,
                enable_attempts: 0,
                enabled: BTreeSet::new(),
                scripted_reads: VecDeque::new(),
            }
        }

        fn with_reads(reads: impl IntoIterator<Item = PerfReadRecord>) -> Self {
            Self {
                scripted_reads: reads.into_iter().collect(),
                ..Self::new()
            }
        }
    }

    impl PerfEventOps for FakeOps {
        fn open_event(
            &mut self,
            _attr: &PerfEventAttrV1,
            _cpu: i32,
            group_fd: RawFd,
            _context: &str,
        ) -> Result<OwnedFd> {
            if group_fd == -1 {
                self.calls.push("open:-1");
            } else {
                self.calls.push("open:leader");
            }
            let file = File::open("/dev/null").expect("open /dev/null");
            Ok(OwnedFd::from(file))
        }

        fn ioctl_group(
            &mut self,
            fd: &OwnedFd,
            request: libc::c_ulong,
            context: &str,
        ) -> Result<()> {
            match request {
                PERF_EVENT_IOC_RESET => self.calls.push("ioctl:reset"),
                PERF_EVENT_IOC_ENABLE => {
                    self.calls.push("ioctl:enable");
                    self.enable_attempts += 1;
                    if self.fail_enable_at == Some(self.enable_attempts) {
                        return Err(map_perf_errno(
                            PerfOperation::Ioctl,
                            context,
                            io::Error::from_raw_os_error(libc::EBUSY),
                        ));
                    }
                    self.enabled.insert(fd.as_raw_fd());
                }
                PERF_EVENT_IOC_DISABLE => {
                    self.calls.push("ioctl:disable");
                    if self.fail_disable {
                        return Err(map_perf_errno(
                            PerfOperation::Ioctl,
                            context,
                            io::Error::from_raw_os_error(libc::EBUSY),
                        ));
                    }
                    self.enabled.remove(&fd.as_raw_fd());
                }
                _ => self.calls.push("ioctl:unknown"),
            }
            Ok(())
        }

        fn read_event(&mut self, _fd: &OwnedFd, context: &str) -> Result<PerfReadRecord> {
            self.calls.push("read");
            self.read_attempts += 1;
            if self.fail_read_at == Some(self.read_attempts) {
                return Err(malformed_perf(context, "read failed"));
            }
            if let Some(record) = self.scripted_reads.pop_front() {
                return Ok(record);
            }
            let value = self.next_read;
            self.next_read += 1;
            Ok(PerfReadRecord {
                value,
                time_enabled: value,
                time_running: value,
            })
        }
    }
}
