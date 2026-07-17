use std::time::Duration;

use crate::cli::{BenchOptions, TimingMode};
use crate::output::CaseOutcome;

use super::analyze::analyze_durations;
use super::command::{
    copy_d2h_sync, copy_h2d_sync, copy_staged_sync, create_timestamp_event, create_timestamp_pool,
    sample_d2d, sample_d2h, sample_h2d, time_staged_sync,
};
use super::error::{CaseExecutionError, ze_fatal};
use super::event::ExecutionEvent;
use super::plan::{CasePlan, ExecutionPlan, STAGED_DEVICE_TIMESTAMP_SKIP_REASON};
use super::sampling::{estimate_collection, sample_capacity, warmup};
use super::saturation;
use super::topology::{DeviceRecord, Topology};
use super::verify::{PATTERN_SEED, SENTINEL_SEED, fill_pattern, verify_or_fail};

const ALLOCATION_ALIGNMENT: usize = 64;
const DEVICE_MEMORY_ORDINAL: u32 = 0;

pub(crate) fn execute_case(
    topology: &Topology,
    plan: &CasePlan,
    options: &BenchOptions,
    bytes: usize,
    events: &mut dyn FnMut(ExecutionEvent),
) -> std::result::Result<CaseOutcome, CaseExecutionError> {
    if options.mode == crate::cli::BenchMode::Saturation {
        let durations = saturation::measure_case(topology, plan, options, bytes, events)?;
        events(ExecutionEvent::Analysis);
        return analyze_durations(options.size_bytes, &durations, &plan.label());
    }

    let durations = match plan.execution {
        ExecutionPlan::HostToDevice { device } => {
            measure_h2d(topology, plan, options, bytes, device, events)?
        }
        ExecutionPlan::DeviceToHost { device } => {
            measure_d2h(topology, plan, options, bytes, device, events)?
        }
        ExecutionPlan::SameDevice { device } => {
            measure_same_device(topology, plan, options, bytes, device, events)?
        }
        ExecutionPlan::Direct {
            source,
            destination,
        } => measure_direct(topology, plan, options, bytes, source, destination, events)?,
        ExecutionPlan::Staged {
            source,
            destination,
        } => measure_staged(topology, plan, options, bytes, source, destination, events)?,
    };

    events(ExecutionEvent::Analysis);
    analyze_durations(options.size_bytes, &durations, &plan.label())
}

fn measure_h2d(
    topology: &Topology,
    plan: &CasePlan,
    options: &BenchOptions,
    bytes: usize,
    device_index: usize,
    events: &mut dyn FnMut(ExecutionEvent),
) -> std::result::Result<Vec<Duration>, CaseExecutionError> {
    let device = &topology.devices[device_index];
    let driver = topology
        .driver_for(device)
        .map_err(CaseExecutionError::Fatal)?;
    let context = ze_fatal(driver.create_context(), "create context")?;
    let queue = ze_fatal(
        context.create_command_queue(&device.device, plan.single_group_ordinal()),
        "create command queue",
    )?;
    let list = ze_fatal(
        context.create_command_list(&device.device, plan.single_group_ordinal()),
        "create command list",
    )?;
    let mut source = ze_fatal(
        context.alloc_host(bytes, ALLOCATION_ALIGNMENT),
        "allocate pinned host source",
    )?;
    let mut verify = ze_fatal(
        context.alloc_host(bytes, ALLOCATION_ALIGNMENT),
        "allocate pinned host verification buffer",
    )?;
    let device_dst = ze_fatal(
        context.alloc_device(
            &device.device,
            bytes,
            ALLOCATION_ALIGNMENT,
            DEVICE_MEMORY_ORDINAL,
        ),
        "allocate device destination",
    )?;
    let timestamp_pool = create_timestamp_pool(&context, options.timing, &[&device.device])?;
    let timestamp_event = create_timestamp_event(timestamp_pool.as_ref())?;

    fill_pattern(source.as_mut_slice(), PATTERN_SEED);
    events(ExecutionEvent::Warmup {
        duration: options.warmup,
    });
    let warmup = warmup(options.warmup, || {
        sample_h2d(
            options.timing,
            &queue,
            &list,
            &device_dst,
            &source,
            bytes,
            timestamp_event.as_ref(),
            &device.properties,
            plan,
        )
        .map(|_| ())
    })?;
    events(ExecutionEvent::Sampling {
        samples: options.samples,
        estimated: estimate_collection(warmup, options.samples),
    });

    let label = plan.label();
    let mut durations = Vec::with_capacity(sample_capacity(options.samples)?);
    for sample_index in 0..options.samples {
        fill_pattern(verify.as_mut_slice(), SENTINEL_SEED);
        ze_fatal(
            copy_h2d_sync(&queue, &list, &device_dst, &verify, bytes),
            "clear device destination before sample",
        )?;

        let elapsed = sample_h2d(
            options.timing,
            &queue,
            &list,
            &device_dst,
            &source,
            bytes,
            timestamp_event.as_ref(),
            &device.properties,
            plan,
        )?;

        ze_fatal(
            copy_d2h_sync(&queue, &list, &mut verify, &device_dst, bytes),
            "copy device destination for verification",
        )?;
        verify_or_fail(verify.as_slice(), PATTERN_SEED, &label)?;
        durations.push(elapsed);
        events(ExecutionEvent::SamplingProgress {
            completed: sample_index + 1,
            total: options.samples,
        });
    }

    Ok(durations)
}

fn measure_d2h(
    topology: &Topology,
    plan: &CasePlan,
    options: &BenchOptions,
    bytes: usize,
    device_index: usize,
    events: &mut dyn FnMut(ExecutionEvent),
) -> std::result::Result<Vec<Duration>, CaseExecutionError> {
    let device = &topology.devices[device_index];
    let driver = topology
        .driver_for(device)
        .map_err(CaseExecutionError::Fatal)?;
    let context = ze_fatal(driver.create_context(), "create context")?;
    let queue = ze_fatal(
        context.create_command_queue(&device.device, plan.single_group_ordinal()),
        "create command queue",
    )?;
    let list = ze_fatal(
        context.create_command_list(&device.device, plan.single_group_ordinal()),
        "create command list",
    )?;
    let mut source = ze_fatal(
        context.alloc_host(bytes, ALLOCATION_ALIGNMENT),
        "allocate pinned host source",
    )?;
    let mut destination = ze_fatal(
        context.alloc_host(bytes, ALLOCATION_ALIGNMENT),
        "allocate pinned host destination",
    )?;
    let device_src = ze_fatal(
        context.alloc_device(
            &device.device,
            bytes,
            ALLOCATION_ALIGNMENT,
            DEVICE_MEMORY_ORDINAL,
        ),
        "allocate device source",
    )?;
    let timestamp_pool = create_timestamp_pool(&context, options.timing, &[&device.device])?;
    let timestamp_event = create_timestamp_event(timestamp_pool.as_ref())?;

    fill_pattern(source.as_mut_slice(), PATTERN_SEED);
    ze_fatal(
        copy_h2d_sync(&queue, &list, &device_src, &source, bytes),
        "initialize device source",
    )?;
    events(ExecutionEvent::Warmup {
        duration: options.warmup,
    });
    let warmup = warmup(options.warmup, || {
        sample_d2h(
            options.timing,
            &queue,
            &list,
            &mut destination,
            &device_src,
            bytes,
            timestamp_event.as_ref(),
            &device.properties,
            plan,
        )
        .map(|_| ())
    })?;
    events(ExecutionEvent::Sampling {
        samples: options.samples,
        estimated: estimate_collection(warmup, options.samples),
    });

    let label = plan.label();
    let mut durations = Vec::with_capacity(sample_capacity(options.samples)?);
    for sample_index in 0..options.samples {
        fill_pattern(destination.as_mut_slice(), SENTINEL_SEED);
        let elapsed = sample_d2h(
            options.timing,
            &queue,
            &list,
            &mut destination,
            &device_src,
            bytes,
            timestamp_event.as_ref(),
            &device.properties,
            plan,
        )?;
        verify_or_fail(destination.as_slice(), PATTERN_SEED, &label)?;
        durations.push(elapsed);
        events(ExecutionEvent::SamplingProgress {
            completed: sample_index + 1,
            total: options.samples,
        });
    }

    Ok(durations)
}

fn measure_same_device(
    topology: &Topology,
    plan: &CasePlan,
    options: &BenchOptions,
    bytes: usize,
    device_index: usize,
    events: &mut dyn FnMut(ExecutionEvent),
) -> std::result::Result<Vec<Duration>, CaseExecutionError> {
    let device = &topology.devices[device_index];
    let driver = topology
        .driver_for(device)
        .map_err(CaseExecutionError::Fatal)?;
    let context = ze_fatal(driver.create_context(), "create context")?;
    let queue = ze_fatal(
        context.create_command_queue(&device.device, plan.single_group_ordinal()),
        "create command queue",
    )?;
    let list = ze_fatal(
        context.create_command_list(&device.device, plan.single_group_ordinal()),
        "create command list",
    )?;
    let mut host = ze_fatal(
        context.alloc_host(bytes, ALLOCATION_ALIGNMENT),
        "allocate pinned host source/verification buffer",
    )?;
    let device_src = ze_fatal(
        context.alloc_device(
            &device.device,
            bytes,
            ALLOCATION_ALIGNMENT,
            DEVICE_MEMORY_ORDINAL,
        ),
        "allocate device source",
    )?;
    let device_dst = ze_fatal(
        context.alloc_device(
            &device.device,
            bytes,
            ALLOCATION_ALIGNMENT,
            DEVICE_MEMORY_ORDINAL,
        ),
        "allocate device destination",
    )?;
    let timestamp_pool = create_timestamp_pool(&context, options.timing, &[&device.device])?;
    let timestamp_event = create_timestamp_event(timestamp_pool.as_ref())?;

    fill_pattern(host.as_mut_slice(), PATTERN_SEED);
    ze_fatal(
        copy_h2d_sync(&queue, &list, &device_src, &host, bytes),
        "initialize device source",
    )?;
    events(ExecutionEvent::Warmup {
        duration: options.warmup,
    });
    let warmup = warmup(options.warmup, || {
        sample_d2d(
            options.timing,
            &queue,
            &list,
            &device_dst,
            &device_src,
            bytes,
            timestamp_event.as_ref(),
            &device.properties,
            plan,
        )
        .map(|_| ())
    })?;
    events(ExecutionEvent::Sampling {
        samples: options.samples,
        estimated: estimate_collection(warmup, options.samples),
    });

    let label = plan.label();
    let mut durations = Vec::with_capacity(sample_capacity(options.samples)?);
    for sample_index in 0..options.samples {
        fill_pattern(host.as_mut_slice(), SENTINEL_SEED);
        ze_fatal(
            copy_h2d_sync(&queue, &list, &device_dst, &host, bytes),
            "clear device destination before sample",
        )?;
        let elapsed = sample_d2d(
            options.timing,
            &queue,
            &list,
            &device_dst,
            &device_src,
            bytes,
            timestamp_event.as_ref(),
            &device.properties,
            plan,
        )?;
        ze_fatal(
            copy_d2h_sync(&queue, &list, &mut host, &device_dst, bytes),
            "copy device destination for verification",
        )?;
        verify_or_fail(host.as_slice(), PATTERN_SEED, &label)?;
        durations.push(elapsed);
        events(ExecutionEvent::SamplingProgress {
            completed: sample_index + 1,
            total: options.samples,
        });
    }

    Ok(durations)
}

#[allow(clippy::too_many_lines)]
fn measure_direct(
    topology: &Topology,
    plan: &CasePlan,
    options: &BenchOptions,
    bytes: usize,
    source_index: usize,
    destination_index: usize,
    events: &mut dyn FnMut(ExecutionEvent),
) -> std::result::Result<Vec<Duration>, CaseExecutionError> {
    let source = &topology.devices[source_index];
    let destination = &topology.devices[destination_index];
    require_shared_driver(source, destination, "direct cross-device copy")?;

    let driver = topology
        .driver_for(source)
        .map_err(CaseExecutionError::Fatal)?;
    let context = ze_fatal(driver.create_context(), "create context")?;
    let source_queue = ze_fatal(
        context.create_command_queue(&source.device, plan.single_group_ordinal()),
        "create source command queue",
    )?;
    let source_list = ze_fatal(
        context.create_command_list(&source.device, plan.single_group_ordinal()),
        "create source command list",
    )?;
    let verification_stream = plan.verification_stream.as_ref().ok_or_else(|| {
        CaseExecutionError::Skip(format!(
            "destination dev{} has no copy-capable engine for verification",
            destination.index
        ))
    })?;
    let destination_queue = ze_fatal(
        context.create_command_queue_at(
            &destination.device,
            verification_stream.group_ordinal,
            verification_stream.queue_index,
        ),
        "create destination verification queue",
    )?;
    let destination_list = ze_fatal(
        context.create_command_list(&destination.device, verification_stream.group_ordinal),
        "create destination verification command list",
    )?;
    let mut host = ze_fatal(
        context.alloc_host(bytes, ALLOCATION_ALIGNMENT),
        "allocate pinned host source/verification buffer",
    )?;
    let device_src = ze_fatal(
        context.alloc_device(
            &source.device,
            bytes,
            ALLOCATION_ALIGNMENT,
            DEVICE_MEMORY_ORDINAL,
        ),
        "allocate source device buffer",
    )?;
    let device_dst = ze_fatal(
        context.alloc_device(
            &destination.device,
            bytes,
            ALLOCATION_ALIGNMENT,
            DEVICE_MEMORY_ORDINAL,
        ),
        "allocate destination device buffer",
    )?;
    let timestamp_pool = create_timestamp_pool(&context, options.timing, &[&source.device])?;
    let timestamp_event = create_timestamp_event(timestamp_pool.as_ref())?;

    fill_pattern(host.as_mut_slice(), PATTERN_SEED);
    ze_fatal(
        copy_h2d_sync(&source_queue, &source_list, &device_src, &host, bytes),
        "initialize source device buffer",
    )?;
    events(ExecutionEvent::Warmup {
        duration: options.warmup,
    });
    let warmup = warmup(options.warmup, || {
        sample_d2d(
            options.timing,
            &source_queue,
            &source_list,
            &device_dst,
            &device_src,
            bytes,
            timestamp_event.as_ref(),
            &source.properties,
            plan,
        )
        .map(|_| ())
    })?;
    events(ExecutionEvent::Sampling {
        samples: options.samples,
        estimated: estimate_collection(warmup, options.samples),
    });

    let label = plan.label();
    let mut durations = Vec::with_capacity(sample_capacity(options.samples)?);
    for sample_index in 0..options.samples {
        fill_pattern(host.as_mut_slice(), SENTINEL_SEED);
        ze_fatal(
            copy_h2d_sync(
                &destination_queue,
                &destination_list,
                &device_dst,
                &host,
                bytes,
            ),
            "clear destination device buffer before sample",
        )?;
        let elapsed = sample_d2d(
            options.timing,
            &source_queue,
            &source_list,
            &device_dst,
            &device_src,
            bytes,
            timestamp_event.as_ref(),
            &source.properties,
            plan,
        )?;
        ze_fatal(
            copy_d2h_sync(
                &destination_queue,
                &destination_list,
                &mut host,
                &device_dst,
                bytes,
            ),
            "copy destination device buffer for verification",
        )?;
        verify_or_fail(host.as_slice(), PATTERN_SEED, &label)?;
        durations.push(elapsed);
        events(ExecutionEvent::SamplingProgress {
            completed: sample_index + 1,
            total: options.samples,
        });
    }

    Ok(durations)
}

#[allow(clippy::too_many_lines)]
fn measure_staged(
    topology: &Topology,
    plan: &CasePlan,
    options: &BenchOptions,
    bytes: usize,
    source_index: usize,
    destination_index: usize,
    events: &mut dyn FnMut(ExecutionEvent),
) -> std::result::Result<Vec<Duration>, CaseExecutionError> {
    if options.timing == TimingMode::DeviceTimestamps {
        return Err(CaseExecutionError::Skip(
            STAGED_DEVICE_TIMESTAMP_SKIP_REASON.to_owned(),
        ));
    }

    let source = &topology.devices[source_index];
    let destination = &topology.devices[destination_index];
    require_shared_driver(source, destination, "explicit staged transfer")?;

    let driver = topology
        .driver_for(source)
        .map_err(CaseExecutionError::Fatal)?;
    let context = ze_fatal(driver.create_context(), "create context")?;
    let source_queue = ze_fatal(
        context.create_command_queue(&source.device, plan.single_group_ordinal()),
        "create source command queue",
    )?;
    let source_list = ze_fatal(
        context.create_command_list(&source.device, plan.single_group_ordinal()),
        "create source command list",
    )?;
    let destination_queue = ze_fatal(
        context.create_command_queue(&destination.device, plan.single_group_ordinal()),
        "create destination command queue",
    )?;
    let destination_list = ze_fatal(
        context.create_command_list(&destination.device, plan.single_group_ordinal()),
        "create destination command list",
    )?;
    let mut host_source = ze_fatal(
        context.alloc_host(bytes, ALLOCATION_ALIGNMENT),
        "allocate pinned host source",
    )?;
    let mut staging = ze_fatal(
        context.alloc_host(bytes, ALLOCATION_ALIGNMENT),
        "allocate pinned host staging buffer",
    )?;
    let mut verify = ze_fatal(
        context.alloc_host(bytes, ALLOCATION_ALIGNMENT),
        "allocate pinned host verification buffer",
    )?;
    let device_src = ze_fatal(
        context.alloc_device(
            &source.device,
            bytes,
            ALLOCATION_ALIGNMENT,
            DEVICE_MEMORY_ORDINAL,
        ),
        "allocate source device buffer",
    )?;
    let device_dst = ze_fatal(
        context.alloc_device(
            &destination.device,
            bytes,
            ALLOCATION_ALIGNMENT,
            DEVICE_MEMORY_ORDINAL,
        ),
        "allocate destination device buffer",
    )?;

    fill_pattern(host_source.as_mut_slice(), PATTERN_SEED);
    ze_fatal(
        copy_h2d_sync(
            &source_queue,
            &source_list,
            &device_src,
            &host_source,
            bytes,
        ),
        "initialize source device buffer",
    )?;
    events(ExecutionEvent::Warmup {
        duration: options.warmup,
    });
    let warmup = warmup(options.warmup, || {
        ze_fatal(
            copy_staged_sync(
                &source_queue,
                &source_list,
                &destination_queue,
                &destination_list,
                &mut staging,
                &device_src,
                &device_dst,
                bytes,
            ),
            "warm up explicit staged copy",
        )
    })?;
    events(ExecutionEvent::Sampling {
        samples: options.samples,
        estimated: estimate_collection(warmup, options.samples),
    });

    let label = plan.label();
    let mut durations = Vec::with_capacity(sample_capacity(options.samples)?);
    for sample_index in 0..options.samples {
        fill_pattern(verify.as_mut_slice(), SENTINEL_SEED);
        ze_fatal(
            copy_h2d_sync(
                &destination_queue,
                &destination_list,
                &device_dst,
                &verify,
                bytes,
            ),
            "clear destination device buffer before sample",
        )?;
        let elapsed = ze_fatal(
            time_staged_sync(
                &source_queue,
                &source_list,
                &destination_queue,
                &destination_list,
                &mut staging,
                &device_src,
                &device_dst,
                bytes,
            ),
            "sample explicit staged copy",
        )?;
        ze_fatal(
            copy_d2h_sync(
                &destination_queue,
                &destination_list,
                &mut verify,
                &device_dst,
                bytes,
            ),
            "copy destination device buffer for verification",
        )?;
        verify_or_fail(verify.as_slice(), PATTERN_SEED, &label)?;
        durations.push(elapsed);
        events(ExecutionEvent::SamplingProgress {
            completed: sample_index + 1,
            total: options.samples,
        });
    }

    Ok(durations)
}

fn require_shared_driver(
    source: &DeviceRecord,
    destination: &DeviceRecord,
    path: &'static str,
) -> std::result::Result<(), CaseExecutionError> {
    if source.driver_index == destination.driver_index {
        Ok(())
    } else {
        Err(CaseExecutionError::Skip(format!(
            "{path} requires both devices in one Level Zero driver/context; dev{} is on driver {} and dev{} is on driver {}",
            source.index, source.driver_index, destination.index, destination.driver_index
        )))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::super::event::ExecutionEvent;

    #[test]
    fn execution_lifecycle_event_order_is_warmup_sampling_analysis() {
        let mut seen = Vec::new();
        let mut events = |event| match event {
            ExecutionEvent::Warmup { .. } => seen.push("warmup"),
            ExecutionEvent::Sampling { .. } => seen.push("sampling"),
            ExecutionEvent::SamplingProgress { .. } => seen.push("progress"),
            ExecutionEvent::Analysis => seen.push("analysis"),
        };

        events(ExecutionEvent::Warmup {
            duration: Duration::from_millis(1),
        });
        events(ExecutionEvent::Sampling {
            samples: 2,
            estimated: Some(Duration::from_millis(1)),
        });
        events(ExecutionEvent::Analysis);

        assert_eq!(seen, ["warmup", "sampling", "analysis"]);
    }
}
