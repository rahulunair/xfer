#![allow(unsafe_code, clippy::too_many_arguments)]

use std::time::{Duration, Instant};

use crate::cli::TimingMode;
use crate::level_zero as ze;
use crate::output::Operation;

use super::error::{BenchmarkError, CaseExecutionError, ze_fatal};
use super::plan::CasePlan;

pub(crate) const QUEUE_SYNC_TIMEOUT_NS: u64 = u64::MAX;

pub(crate) fn sample_h2d(
    timing: TimingMode,
    queue: &ze::CommandQueue<'_>,
    list: &ze::CommandList<'_>,
    dst: &ze::DeviceAllocation<'_>,
    src: &ze::HostAllocation<'_>,
    bytes: usize,
    timestamp_event: Option<&ze::Event<'_>>,
    properties: &ze::DeviceProperties,
    plan: &CasePlan,
) -> std::result::Result<Duration, CaseExecutionError> {
    match timing {
        TimingMode::WallClock => ze_fatal(
            time_h2d_sync(queue, list, dst, src, bytes),
            "sample host-to-device copy",
        ),
        TimingMode::DeviceTimestamps => {
            let event = require_timestamp_event(timestamp_event, plan)?;
            event
                .host_reset()
                .map_err(|error| timestamp_level_zero_error("reset timestamp event", error))?;
            prepare_h2d_list(list, dst, src, bytes, Some(event)).map_err(|error| {
                timestamp_level_zero_error("record timestamped host-to-device copy", error)
            })?;
            queue.execute(&[list]).map_err(|error| {
                timestamp_level_zero_error("execute timestamped host-to-device copy", error)
            })?;
            queue.synchronize(QUEUE_SYNC_TIMEOUT_NS).map_err(|error| {
                timestamp_level_zero_error("synchronize timestamped host-to-device copy", error)
            })?;
            timestamp_duration(event, properties, plan)
        }
    }
}

pub(crate) fn sample_d2h(
    timing: TimingMode,
    queue: &ze::CommandQueue<'_>,
    list: &ze::CommandList<'_>,
    dst: &mut ze::HostAllocation<'_>,
    src: &ze::DeviceAllocation<'_>,
    bytes: usize,
    timestamp_event: Option<&ze::Event<'_>>,
    properties: &ze::DeviceProperties,
    plan: &CasePlan,
) -> std::result::Result<Duration, CaseExecutionError> {
    match timing {
        TimingMode::WallClock => ze_fatal(
            time_d2h_sync(queue, list, dst, src, bytes),
            "sample device-to-host copy",
        ),
        TimingMode::DeviceTimestamps => {
            let event = require_timestamp_event(timestamp_event, plan)?;
            event
                .host_reset()
                .map_err(|error| timestamp_level_zero_error("reset timestamp event", error))?;
            prepare_d2h_list(list, dst, src, bytes, Some(event)).map_err(|error| {
                timestamp_level_zero_error("record timestamped device-to-host copy", error)
            })?;
            queue.execute(&[list]).map_err(|error| {
                timestamp_level_zero_error("execute timestamped device-to-host copy", error)
            })?;
            queue.synchronize(QUEUE_SYNC_TIMEOUT_NS).map_err(|error| {
                timestamp_level_zero_error("synchronize timestamped device-to-host copy", error)
            })?;
            timestamp_duration(event, properties, plan)
        }
    }
}

pub(crate) fn sample_d2d(
    timing: TimingMode,
    queue: &ze::CommandQueue<'_>,
    list: &ze::CommandList<'_>,
    dst: &ze::DeviceAllocation<'_>,
    src: &ze::DeviceAllocation<'_>,
    bytes: usize,
    timestamp_event: Option<&ze::Event<'_>>,
    properties: &ze::DeviceProperties,
    plan: &CasePlan,
) -> std::result::Result<Duration, CaseExecutionError> {
    match timing {
        TimingMode::WallClock if matches!(plan.operation, Operation::Direct { .. }) => {
            time_direct_d2d_sync(queue, list, dst, src, bytes)
        }
        TimingMode::WallClock => ze_fatal(
            time_d2d_sync(queue, list, dst, src, bytes),
            "sample device-to-device copy",
        ),
        TimingMode::DeviceTimestamps => {
            let event = require_timestamp_event(timestamp_event, plan)?;
            event
                .host_reset()
                .map_err(|error| timestamp_level_zero_error("reset timestamp event", error))?;
            let record_phase = if matches!(plan.operation, Operation::Direct { .. }) {
                "record direct device-to-device copy"
            } else {
                "record timestamped device-to-device copy"
            };
            let execute_phase = if matches!(plan.operation, Operation::Direct { .. }) {
                "execute direct device-to-device copy"
            } else {
                "execute timestamped device-to-device copy"
            };
            let sync_phase = if matches!(plan.operation, Operation::Direct { .. }) {
                "synchronize direct device-to-device copy"
            } else {
                "synchronize timestamped device-to-device copy"
            };
            prepare_d2d_list(list, dst, src, bytes, Some(event))
                .map_err(|error| timestamp_level_zero_error(record_phase, error))?;
            queue
                .execute(&[list])
                .map_err(|error| timestamp_level_zero_error(execute_phase, error))?;
            queue
                .synchronize(QUEUE_SYNC_TIMEOUT_NS)
                .map_err(|error| timestamp_level_zero_error(sync_phase, error))?;
            timestamp_duration(event, properties, plan)
        }
    }
}

fn time_direct_d2d_sync(
    queue: &ze::CommandQueue<'_>,
    list: &ze::CommandList<'_>,
    dst: &ze::DeviceAllocation<'_>,
    src: &ze::DeviceAllocation<'_>,
    bytes: usize,
) -> std::result::Result<Duration, CaseExecutionError> {
    prepare_d2d_list(list, dst, src, bytes, None).map_err(|error| {
        timestamp_level_zero_error("record direct device-to-device copy", error)
    })?;
    let started = Instant::now();
    queue.execute(&[list]).map_err(|error| {
        timestamp_level_zero_error("execute direct device-to-device copy", error)
    })?;
    queue.synchronize(QUEUE_SYNC_TIMEOUT_NS).map_err(|error| {
        timestamp_level_zero_error("synchronize direct device-to-device copy", error)
    })?;
    Ok(started.elapsed())
}

pub(crate) fn create_timestamp_pool<'context>(
    context: &'context ze::Context<'_>,
    timing: TimingMode,
    devices: &[&ze::Device],
) -> std::result::Result<Option<ze::EventPool<'context>>, CaseExecutionError> {
    if timing == TimingMode::DeviceTimestamps {
        ze_fatal(
            ze::EventPool::kernel_timestamps(context, devices, 1),
            "create timestamp event pool",
        )
        .map(Some)
    } else {
        Ok(None)
    }
}

pub(crate) fn create_timestamp_event<'pool>(
    pool: Option<&'pool ze::EventPool<'_>>,
) -> std::result::Result<Option<ze::Event<'pool>>, CaseExecutionError> {
    pool.map(|pool| ze_fatal(pool.create_event(0), "create timestamp event"))
        .transpose()
}

fn require_timestamp_event<'event>(
    event: Option<&'event ze::Event<'_>>,
    plan: &CasePlan,
) -> std::result::Result<&'event ze::Event<'event>, CaseExecutionError> {
    event.ok_or_else(|| {
        CaseExecutionError::Fatal(BenchmarkError::Topology(format!(
            "{} missing timestamp event for device timestamp sample",
            plan.label()
        )))
    })
}

fn timestamp_duration(
    event: &ze::Event<'_>,
    properties: &ze::DeviceProperties,
    plan: &CasePlan,
) -> std::result::Result<Duration, CaseExecutionError> {
    let timestamp = ze_fatal(event.query_kernel_timestamp(), "query timestamp event")?;
    let ticks = elapsed_timestamp_ticks(
        timestamp.global_kernel_start,
        timestamp.global_kernel_end,
        properties.kernel_timestamp_valid_bits,
    );
    let nanos = u128::from(ticks) * u128::from(properties.timer_resolution);
    let nanos = u64::try_from(nanos).map_err(|_| {
        CaseExecutionError::Fatal(BenchmarkError::Statistics(format!(
            "{} device timestamp duration overflowed u64 nanoseconds",
            plan.label()
        )))
    })?;
    Ok(Duration::from_nanos(nanos))
}

fn timestamp_level_zero_error(
    phase: &'static str,
    error: ze::LevelZeroError,
) -> CaseExecutionError {
    CaseExecutionError::Fatal(BenchmarkError::LevelZeroOperation { phase, error })
}

pub(crate) fn copy_h2d_sync(
    queue: &ze::CommandQueue<'_>,
    list: &ze::CommandList<'_>,
    dst: &ze::DeviceAllocation<'_>,
    src: &ze::HostAllocation<'_>,
    bytes: usize,
) -> ze::Result<()> {
    prepare_h2d_list(list, dst, src, bytes, None)?;
    queue.execute(&[list])?;
    queue.synchronize(QUEUE_SYNC_TIMEOUT_NS)
}

fn time_h2d_sync(
    queue: &ze::CommandQueue<'_>,
    list: &ze::CommandList<'_>,
    dst: &ze::DeviceAllocation<'_>,
    src: &ze::HostAllocation<'_>,
    bytes: usize,
) -> ze::Result<Duration> {
    prepare_h2d_list(list, dst, src, bytes, None)?;
    let started = Instant::now();
    queue.execute(&[list])?;
    queue.synchronize(QUEUE_SYNC_TIMEOUT_NS)?;
    Ok(started.elapsed())
}

pub(crate) fn prepare_h2d_list(
    list: &ze::CommandList<'_>,
    dst: &ze::DeviceAllocation<'_>,
    src: &ze::HostAllocation<'_>,
    bytes: usize,
    signal: Option<&ze::Event<'_>>,
) -> ze::Result<()> {
    prepare_h2d_region(list, dst, 0, src, 0, bytes, signal)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare_h2d_region(
    list: &ze::CommandList<'_>,
    dst: &ze::DeviceAllocation<'_>,
    dst_offset: usize,
    src: &ze::HostAllocation<'_>,
    src_offset: usize,
    bytes: usize,
    signal: Option<&ze::Event<'_>>,
) -> ze::Result<()> {
    list.reset()?;
    unsafe {
        // SAFETY: source and destination allocations outlive queue execution; offsets and byte
        // count are bounded by the wrapper, and callers synchronize before reuse or host access.
        list.append_host_to_device_region(dst, dst_offset, src, src_offset, bytes, signal, &[])?;
    }
    list.close()
}

pub(crate) fn copy_d2h_sync(
    queue: &ze::CommandQueue<'_>,
    list: &ze::CommandList<'_>,
    dst: &mut ze::HostAllocation<'_>,
    src: &ze::DeviceAllocation<'_>,
    bytes: usize,
) -> ze::Result<()> {
    prepare_d2h_list(list, dst, src, bytes, None)?;
    queue.execute(&[list])?;
    queue.synchronize(QUEUE_SYNC_TIMEOUT_NS)
}

fn time_d2h_sync(
    queue: &ze::CommandQueue<'_>,
    list: &ze::CommandList<'_>,
    dst: &mut ze::HostAllocation<'_>,
    src: &ze::DeviceAllocation<'_>,
    bytes: usize,
) -> ze::Result<Duration> {
    prepare_d2h_list(list, dst, src, bytes, None)?;
    let started = Instant::now();
    queue.execute(&[list])?;
    queue.synchronize(QUEUE_SYNC_TIMEOUT_NS)?;
    Ok(started.elapsed())
}

pub(crate) fn prepare_d2h_list(
    list: &ze::CommandList<'_>,
    dst: &mut ze::HostAllocation<'_>,
    src: &ze::DeviceAllocation<'_>,
    bytes: usize,
    signal: Option<&ze::Event<'_>>,
) -> ze::Result<()> {
    prepare_d2h_region(list, dst, 0, src, 0, bytes, signal)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare_d2h_region(
    list: &ze::CommandList<'_>,
    dst: &mut ze::HostAllocation<'_>,
    dst_offset: usize,
    src: &ze::DeviceAllocation<'_>,
    src_offset: usize,
    bytes: usize,
    signal: Option<&ze::Event<'_>>,
) -> ze::Result<()> {
    list.reset()?;
    unsafe {
        // SAFETY: source and destination allocations outlive queue execution; offsets and byte
        // count are bounded by the wrapper, and callers synchronize before reuse or host access.
        list.append_device_to_host_region(dst, dst_offset, src, src_offset, bytes, signal, &[])?;
    }
    list.close()
}

fn time_d2d_sync(
    queue: &ze::CommandQueue<'_>,
    list: &ze::CommandList<'_>,
    dst: &ze::DeviceAllocation<'_>,
    src: &ze::DeviceAllocation<'_>,
    bytes: usize,
) -> ze::Result<Duration> {
    prepare_d2d_list(list, dst, src, bytes, None)?;
    let started = Instant::now();
    queue.execute(&[list])?;
    queue.synchronize(QUEUE_SYNC_TIMEOUT_NS)?;
    Ok(started.elapsed())
}

pub(crate) fn prepare_d2d_list(
    list: &ze::CommandList<'_>,
    dst: &ze::DeviceAllocation<'_>,
    src: &ze::DeviceAllocation<'_>,
    bytes: usize,
    signal: Option<&ze::Event<'_>>,
) -> ze::Result<()> {
    prepare_d2d_region(list, dst, 0, src, 0, bytes, signal)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare_d2d_region(
    list: &ze::CommandList<'_>,
    dst: &ze::DeviceAllocation<'_>,
    dst_offset: usize,
    src: &ze::DeviceAllocation<'_>,
    src_offset: usize,
    bytes: usize,
    signal: Option<&ze::Event<'_>>,
) -> ze::Result<()> {
    list.reset()?;
    unsafe {
        // SAFETY: source and destination allocations outlive queue execution; offsets and byte
        // count are bounded by the wrapper, and callers synchronize before reuse or allocation access.
        list.append_device_to_device_region(dst, dst_offset, src, src_offset, bytes, signal, &[])?;
    }
    list.close()
}

pub(crate) fn copy_staged_sync(
    source_queue: &ze::CommandQueue<'_>,
    source_list: &ze::CommandList<'_>,
    destination_queue: &ze::CommandQueue<'_>,
    destination_list: &ze::CommandList<'_>,
    staging: &mut ze::HostAllocation<'_>,
    source: &ze::DeviceAllocation<'_>,
    destination: &ze::DeviceAllocation<'_>,
    bytes: usize,
) -> ze::Result<()> {
    prepare_staged_lists(
        source_list,
        destination_list,
        staging,
        source,
        destination,
        bytes,
    )?;
    source_queue.execute(&[source_list])?;
    source_queue.synchronize(QUEUE_SYNC_TIMEOUT_NS)?;
    destination_queue.execute(&[destination_list])?;
    destination_queue.synchronize(QUEUE_SYNC_TIMEOUT_NS)
}

pub(crate) fn time_staged_sync(
    source_queue: &ze::CommandQueue<'_>,
    source_list: &ze::CommandList<'_>,
    destination_queue: &ze::CommandQueue<'_>,
    destination_list: &ze::CommandList<'_>,
    staging: &mut ze::HostAllocation<'_>,
    source: &ze::DeviceAllocation<'_>,
    destination: &ze::DeviceAllocation<'_>,
    bytes: usize,
) -> ze::Result<Duration> {
    prepare_staged_lists(
        source_list,
        destination_list,
        staging,
        source,
        destination,
        bytes,
    )?;
    let started = Instant::now();
    source_queue.execute(&[source_list])?;
    // The explicit staged sample is end-to-end: D2H must complete before the
    // H2D leg reads the pinned staging buffer.
    source_queue.synchronize(QUEUE_SYNC_TIMEOUT_NS)?;
    destination_queue.execute(&[destination_list])?;
    destination_queue.synchronize(QUEUE_SYNC_TIMEOUT_NS)?;
    Ok(started.elapsed())
}

fn prepare_staged_lists(
    source_list: &ze::CommandList<'_>,
    destination_list: &ze::CommandList<'_>,
    staging: &mut ze::HostAllocation<'_>,
    source: &ze::DeviceAllocation<'_>,
    destination: &ze::DeviceAllocation<'_>,
    bytes: usize,
) -> ze::Result<()> {
    prepare_d2h_list(source_list, staging, source, bytes, None)?;
    prepare_h2d_list(destination_list, destination, staging, bytes, None)
}

fn elapsed_timestamp_ticks(start: u64, end: u64, valid_bits: u32) -> u64 {
    let mask = timestamp_mask(valid_bits);
    (end.wrapping_sub(start)) & mask
}

fn timestamp_mask(valid_bits: u32) -> u64 {
    match valid_bits {
        0 | 64.. => u64::MAX,
        bits => (1_u64 << bits) - 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_ticks_use_valid_bit_wrap() {
        assert_eq!(elapsed_timestamp_ticks(10, 15, 8), 5);
        assert_eq!(elapsed_timestamp_ticks(250, 4, 8), 10);
        assert_eq!(elapsed_timestamp_ticks(u64::MAX - 2, 4, 0), 7);
        assert_eq!(timestamp_mask(64), u64::MAX);
        assert_eq!(timestamp_mask(65), u64::MAX);
    }
}
