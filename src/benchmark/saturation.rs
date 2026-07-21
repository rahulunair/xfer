use std::time::{Duration, Instant};

use crate::cli::BenchOptions;
use crate::level_zero as ze;
use crate::output::QueueStreamInfo;

use super::command::{
    QUEUE_SYNC_TIMEOUT_NS, copy_d2h_sync, copy_h2d_sync, prepare_d2d_region, prepare_d2h_region,
    prepare_h2d_region,
};
use super::error::{BenchmarkError, CaseExecutionError, ze_fatal};
use super::event::ExecutionEvent;
use super::measurement::{MeasurementObserver, SampleContext, observe_sample, sample_context};
use super::plan::{CasePlan, ExecutionPlan};
use super::sampling::{estimate_collection, sample_capacity, warmup};
use super::topology::{DeviceRecord, Topology};
use super::verify::{PATTERN_SEED, SENTINEL_SEED, fill_pattern, verify_or_fail};

const ALLOCATION_ALIGNMENT: usize = 64;
const DEVICE_MEMORY_ORDINAL: u32 = 0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Chunk {
    offset: usize,
    bytes: usize,
}

struct QueueStream<'context> {
    queue: ze::CommandQueue<'context>,
    list: ze::CommandList<'context>,
}

struct StreamSet<'context> {
    streams: Vec<QueueStream<'context>>,
}

impl<'context> StreamSet<'context> {
    fn new(
        context: &'context ze::Context<'_>,
        device: &DeviceRecord,
        streams: &[QueueStreamInfo],
    ) -> std::result::Result<Self, CaseExecutionError> {
        let mut created = Vec::with_capacity(streams.len());
        for stream in streams {
            let queue = ze_fatal(
                context.create_command_queue_at(
                    &device.device,
                    stream.group_ordinal,
                    stream.queue_index,
                ),
                "create saturation command queue",
            )?;
            let list = ze_fatal(
                context.create_command_list(&device.device, stream.group_ordinal),
                "create saturation command list",
            )?;
            created.push(QueueStream { queue, list });
        }
        Ok(Self { streams: created })
    }

    fn chunks(&self, bytes: usize) -> std::result::Result<Vec<Chunk>, CaseExecutionError> {
        partition_bytes(bytes, self.streams.len())
    }

    fn time_prepared(
        &self,
        execute_phase: &'static str,
        synchronize_phase: &'static str,
    ) -> std::result::Result<Duration, CaseExecutionError> {
        let started = Instant::now();
        self.submit_and_sync(execute_phase, synchronize_phase)?;
        Ok(started.elapsed())
    }

    fn submit_and_sync(
        &self,
        execute_phase: &'static str,
        synchronize_phase: &'static str,
    ) -> std::result::Result<(), CaseExecutionError> {
        for (submitted, stream) in self.streams.iter().enumerate() {
            if let Err(error) = stream.queue.execute(&[&stream.list]) {
                self.synchronize_best_effort(submitted);
                return Err(level_zero_phase(execute_phase, error));
            }
        }

        for (index, stream) in self.streams.iter().enumerate() {
            if let Err(error) = stream.queue.synchronize(QUEUE_SYNC_TIMEOUT_NS) {
                self.synchronize_range_best_effort(index + 1);
                return Err(level_zero_phase(synchronize_phase, error));
            }
        }
        Ok(())
    }

    fn synchronize_best_effort(&self, count: usize) {
        for stream in &self.streams[..count] {
            let _ = stream.queue.synchronize(QUEUE_SYNC_TIMEOUT_NS);
        }
    }

    fn synchronize_range_best_effort(&self, start: usize) {
        for stream in &self.streams[start..] {
            let _ = stream.queue.synchronize(QUEUE_SYNC_TIMEOUT_NS);
        }
    }
}

pub(crate) fn measure_case(
    topology: &Topology,
    plan: &CasePlan,
    options: &BenchOptions,
    bytes: usize,
    events: &mut dyn FnMut(ExecutionEvent),
    observer: &mut dyn MeasurementObserver,
) -> std::result::Result<Vec<Duration>, CaseExecutionError> {
    match plan.execution {
        ExecutionPlan::HostToDevice { device } => {
            measure_h2d(topology, plan, options, bytes, device, events)
        }
        ExecutionPlan::DeviceToHost { device } => {
            measure_d2h(topology, plan, options, bytes, device, events)
        }
        ExecutionPlan::SameDevice { device } => {
            measure_same_device(topology, plan, options, bytes, device, events)
        }
        ExecutionPlan::Direct {
            source,
            destination,
        } => measure_direct(
            topology,
            plan,
            options,
            bytes,
            source,
            destination,
            events,
            observer,
        ),
        ExecutionPlan::Staged {
            source,
            destination,
        } => measure_staged(
            topology,
            plan,
            options,
            bytes,
            source,
            destination,
            events,
            observer,
        ),
    }
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
    let context = ze_fatal(driver.create_context(), "create saturation context")?;
    let streams = StreamSet::new(&context, device, &plan.streams)?;
    let chunks = streams.chunks(bytes)?;
    let mut source = alloc_host(&context, bytes, "allocate saturation H2D source")?;
    let mut verify = alloc_host(&context, bytes, "allocate saturation H2D verification")?;
    let destination = alloc_device(
        &context,
        device,
        bytes,
        "allocate saturation H2D destination",
    )?;
    fill_pattern(source.as_mut_slice(), PATTERN_SEED);

    let warmup_result = run_warmup(options, events, || {
        sample_h2d(&streams, &chunks, &destination, &source)
    })?;
    begin_sampling(options, events, warmup_result);
    collect_samples(options, events, || {
        fill_pattern(verify.as_mut_slice(), SENTINEL_SEED);
        copy_h2d_full(
            &streams.streams[0],
            &destination,
            &verify,
            bytes,
            "clear saturation H2D destination",
        )?;
        let elapsed = sample_h2d(&streams, &chunks, &destination, &source)?;
        copy_d2h_full(
            &streams.streams[0],
            &mut verify,
            &destination,
            bytes,
            "verify saturation H2D destination",
        )?;
        verify_or_fail(verify.as_slice(), PATTERN_SEED, &plan.label())?;
        Ok(elapsed)
    })
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
    let context = ze_fatal(driver.create_context(), "create saturation context")?;
    let streams = StreamSet::new(&context, device, &plan.streams)?;
    let chunks = streams.chunks(bytes)?;
    let mut source_host = alloc_host(&context, bytes, "allocate saturation D2H source")?;
    let mut destination = alloc_host(&context, bytes, "allocate saturation D2H destination")?;
    let source = alloc_device(
        &context,
        device,
        bytes,
        "allocate saturation D2H device source",
    )?;
    fill_pattern(source_host.as_mut_slice(), PATTERN_SEED);
    copy_h2d_full(
        &streams.streams[0],
        &source,
        &source_host,
        bytes,
        "initialize saturation D2H source",
    )?;

    let warmup_result = run_warmup(options, events, || {
        sample_d2h(&streams, &chunks, &mut destination, &source)
    })?;
    begin_sampling(options, events, warmup_result);
    collect_samples(options, events, || {
        fill_pattern(destination.as_mut_slice(), SENTINEL_SEED);
        let elapsed = sample_d2h(&streams, &chunks, &mut destination, &source)?;
        verify_or_fail(destination.as_slice(), PATTERN_SEED, &plan.label())?;
        Ok(elapsed)
    })
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
    let context = ze_fatal(driver.create_context(), "create saturation context")?;
    let streams = StreamSet::new(&context, device, &plan.streams)?;
    let chunks = streams.chunks(bytes)?;
    let mut host = alloc_host(
        &context,
        bytes,
        "allocate saturation same-device verification",
    )?;
    let source = alloc_device(
        &context,
        device,
        bytes,
        "allocate saturation same-device source",
    )?;
    let destination = alloc_device(
        &context,
        device,
        bytes,
        "allocate saturation same-device destination",
    )?;
    fill_pattern(host.as_mut_slice(), PATTERN_SEED);
    copy_h2d_full(
        &streams.streams[0],
        &source,
        &host,
        bytes,
        "initialize saturation same-device source",
    )?;

    let warmup_result = run_warmup(options, events, || {
        sample_d2d(&streams, &chunks, &destination, &source)
    })?;
    begin_sampling(options, events, warmup_result);
    collect_samples(options, events, || {
        fill_pattern(host.as_mut_slice(), SENTINEL_SEED);
        copy_h2d_full(
            &streams.streams[0],
            &destination,
            &host,
            bytes,
            "clear saturation same-device destination",
        )?;
        let elapsed = sample_d2d(&streams, &chunks, &destination, &source)?;
        copy_d2h_full(
            &streams.streams[0],
            &mut host,
            &destination,
            bytes,
            "verify saturation same-device destination",
        )?;
        verify_or_fail(host.as_slice(), PATTERN_SEED, &plan.label())?;
        Ok(elapsed)
    })
}

#[allow(clippy::too_many_arguments)]
fn measure_direct(
    topology: &Topology,
    plan: &CasePlan,
    options: &BenchOptions,
    bytes: usize,
    source_index: usize,
    destination_index: usize,
    events: &mut dyn FnMut(ExecutionEvent),
    observer: &mut dyn MeasurementObserver,
) -> std::result::Result<Vec<Duration>, CaseExecutionError> {
    let source_device = &topology.devices[source_index];
    let destination_device = &topology.devices[destination_index];
    require_shared_driver(source_device, destination_device, "saturation direct copy")?;
    let driver = topology
        .driver_for(source_device)
        .map_err(CaseExecutionError::Fatal)?;
    let context = ze_fatal(driver.create_context(), "create saturation context")?;
    let streams = StreamSet::new(&context, source_device, &plan.streams)?;
    let verify_stream = verification_stream(
        &context,
        destination_device,
        plan.verification_stream.as_ref(),
    )?;
    let chunks = streams.chunks(bytes)?;
    let mut host = alloc_host(&context, bytes, "allocate saturation direct verification")?;
    let source = alloc_device(
        &context,
        source_device,
        bytes,
        "allocate saturation direct source",
    )?;
    let destination = alloc_device(
        &context,
        destination_device,
        bytes,
        "allocate saturation direct destination",
    )?;
    fill_pattern(host.as_mut_slice(), PATTERN_SEED);
    copy_h2d_full(
        &streams.streams[0],
        &source,
        &host,
        bytes,
        "initialize saturation direct source",
    )?;

    let warmup_result = run_warmup(options, events, || {
        sample_d2d(&streams, &chunks, &destination, &source)
    })?;
    begin_sampling(options, events, warmup_result);
    collect_observed_samples(
        options,
        events,
        crate::cli::TransferClass::D2DDirect,
        |context| {
            fill_pattern(host.as_mut_slice(), SENTINEL_SEED);
            copy_h2d_full(
                &verify_stream,
                &destination,
                &host,
                bytes,
                "clear saturation direct destination",
            )?;
            let elapsed =
                sample_d2d_observed(&streams, &chunks, &destination, &source, context, observer)?;
            copy_d2h_full(
                &verify_stream,
                &mut host,
                &destination,
                bytes,
                "verify saturation direct destination",
            )?;
            verify_or_fail(host.as_slice(), PATTERN_SEED, &plan.label())?;
            Ok(elapsed)
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn measure_staged(
    topology: &Topology,
    plan: &CasePlan,
    options: &BenchOptions,
    bytes: usize,
    source_index: usize,
    destination_index: usize,
    events: &mut dyn FnMut(ExecutionEvent),
    observer: &mut dyn MeasurementObserver,
) -> std::result::Result<Vec<Duration>, CaseExecutionError> {
    let source_device = &topology.devices[source_index];
    let destination_device = &topology.devices[destination_index];
    require_shared_driver(source_device, destination_device, "saturation staged copy")?;
    let driver = topology
        .driver_for(source_device)
        .map_err(CaseExecutionError::Fatal)?;
    let context = ze_fatal(driver.create_context(), "create saturation context")?;
    let source_streams = StreamSet::new(&context, source_device, &plan.streams)?;
    let destination_streams =
        StreamSet::new(&context, destination_device, &plan.second_phase_streams)?;
    let source_chunks = source_streams.chunks(bytes)?;
    let destination_chunks = destination_streams.chunks(bytes)?;
    let mut source_host = alloc_host(&context, bytes, "allocate saturation staged source")?;
    let mut staging = alloc_host(&context, bytes, "allocate saturation staging buffer")?;
    let mut verify = alloc_host(&context, bytes, "allocate saturation staged verification")?;
    let source = alloc_device(
        &context,
        source_device,
        bytes,
        "allocate saturation staged device source",
    )?;
    let destination = alloc_device(
        &context,
        destination_device,
        bytes,
        "allocate saturation staged device destination",
    )?;
    fill_pattern(source_host.as_mut_slice(), PATTERN_SEED);
    copy_h2d_full(
        &source_streams.streams[0],
        &source,
        &source_host,
        bytes,
        "initialize saturation staged source",
    )?;

    let warmup_result = run_warmup(options, events, || {
        sample_staged(
            &source_streams,
            &source_chunks,
            &destination_streams,
            &destination_chunks,
            &mut staging,
            &source,
            &destination,
        )
    })?;
    begin_sampling(options, events, warmup_result);
    collect_observed_samples(
        options,
        events,
        crate::cli::TransferClass::D2DStaged,
        |context| {
            fill_pattern(verify.as_mut_slice(), SENTINEL_SEED);
            copy_h2d_full(
                &destination_streams.streams[0],
                &destination,
                &verify,
                bytes,
                "clear saturation staged destination",
            )?;
            let elapsed = sample_staged_observed(
                &source_streams,
                &source_chunks,
                &destination_streams,
                &destination_chunks,
                &mut staging,
                &source,
                &destination,
                context,
                observer,
            )?;
            copy_d2h_full(
                &destination_streams.streams[0],
                &mut verify,
                &destination,
                bytes,
                "verify saturation staged destination",
            )?;
            verify_or_fail(verify.as_slice(), PATTERN_SEED, &plan.label())?;
            Ok(elapsed)
        },
    )
}

fn sample_h2d(
    streams: &StreamSet<'_>,
    chunks: &[Chunk],
    destination: &ze::DeviceAllocation<'_>,
    source: &ze::HostAllocation<'_>,
) -> std::result::Result<Duration, CaseExecutionError> {
    for (stream, chunk) in streams.streams.iter().zip(chunks) {
        prepare_h2d_region(
            &stream.list,
            destination,
            chunk.offset,
            source,
            chunk.offset,
            chunk.bytes,
            None,
        )
        .map_err(|error| level_zero_phase("prepare saturation H2D command list", error))?;
    }
    streams.time_prepared(
        "execute saturation H2D copies",
        "synchronize saturation H2D copies",
    )
}

fn sample_d2h(
    streams: &StreamSet<'_>,
    chunks: &[Chunk],
    destination: &mut ze::HostAllocation<'_>,
    source: &ze::DeviceAllocation<'_>,
) -> std::result::Result<Duration, CaseExecutionError> {
    for (stream, chunk) in streams.streams.iter().zip(chunks) {
        prepare_d2h_region(
            &stream.list,
            destination,
            chunk.offset,
            source,
            chunk.offset,
            chunk.bytes,
            None,
        )
        .map_err(|error| level_zero_phase("prepare saturation D2H command list", error))?;
    }
    streams.time_prepared(
        "execute saturation D2H copies",
        "synchronize saturation D2H copies",
    )
}

fn sample_d2d(
    streams: &StreamSet<'_>,
    chunks: &[Chunk],
    destination: &ze::DeviceAllocation<'_>,
    source: &ze::DeviceAllocation<'_>,
) -> std::result::Result<Duration, CaseExecutionError> {
    prepare_d2d_streams(streams, chunks, destination, source)?;
    streams.time_prepared(
        "execute saturation D2D copies",
        "synchronize saturation D2D copies",
    )
}

fn sample_d2d_observed(
    streams: &StreamSet<'_>,
    chunks: &[Chunk],
    destination: &ze::DeviceAllocation<'_>,
    source: &ze::DeviceAllocation<'_>,
    context: &SampleContext,
    observer: &mut dyn MeasurementObserver,
) -> std::result::Result<Duration, CaseExecutionError> {
    prepare_d2d_streams(streams, chunks, destination, source)?;
    observe_sample(observer, context, || {
        streams.time_prepared(
            "execute saturation D2D copies",
            "synchronize saturation D2D copies",
        )
    })
}

fn prepare_d2d_streams(
    streams: &StreamSet<'_>,
    chunks: &[Chunk],
    destination: &ze::DeviceAllocation<'_>,
    source: &ze::DeviceAllocation<'_>,
) -> std::result::Result<(), CaseExecutionError> {
    for (stream, chunk) in streams.streams.iter().zip(chunks) {
        prepare_d2d_region(
            &stream.list,
            destination,
            chunk.offset,
            source,
            chunk.offset,
            chunk.bytes,
            None,
        )
        .map_err(|error| level_zero_phase("prepare saturation D2D command list", error))?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn sample_staged(
    source_streams: &StreamSet<'_>,
    source_chunks: &[Chunk],
    destination_streams: &StreamSet<'_>,
    destination_chunks: &[Chunk],
    staging: &mut ze::HostAllocation<'_>,
    source: &ze::DeviceAllocation<'_>,
    destination: &ze::DeviceAllocation<'_>,
) -> std::result::Result<Duration, CaseExecutionError> {
    prepare_d2h_streams(source_streams, source_chunks, staging, source)?;
    prepare_h2d_streams(
        destination_streams,
        destination_chunks,
        destination,
        staging,
    )?;
    let started = Instant::now();
    source_streams.submit_and_sync(
        "execute saturation staged D2H copies",
        "synchronize saturation staged D2H copies",
    )?;
    destination_streams.submit_and_sync(
        "execute saturation staged H2D copies",
        "synchronize saturation staged H2D copies",
    )?;
    Ok(started.elapsed())
}

#[allow(clippy::too_many_arguments)]
fn sample_staged_observed(
    source_streams: &StreamSet<'_>,
    source_chunks: &[Chunk],
    destination_streams: &StreamSet<'_>,
    destination_chunks: &[Chunk],
    staging: &mut ze::HostAllocation<'_>,
    source: &ze::DeviceAllocation<'_>,
    destination: &ze::DeviceAllocation<'_>,
    context: &SampleContext,
    observer: &mut dyn MeasurementObserver,
) -> std::result::Result<Duration, CaseExecutionError> {
    prepare_d2h_streams(source_streams, source_chunks, staging, source)?;
    prepare_h2d_streams(
        destination_streams,
        destination_chunks,
        destination,
        staging,
    )?;
    observe_sample(observer, context, || {
        let started = Instant::now();
        source_streams.submit_and_sync(
            "execute saturation staged D2H copies",
            "synchronize saturation staged D2H copies",
        )?;
        destination_streams.submit_and_sync(
            "execute saturation staged H2D copies",
            "synchronize saturation staged H2D copies",
        )?;
        Ok(started.elapsed())
    })
}

fn prepare_d2h_streams(
    streams: &StreamSet<'_>,
    chunks: &[Chunk],
    destination: &mut ze::HostAllocation<'_>,
    source: &ze::DeviceAllocation<'_>,
) -> std::result::Result<(), CaseExecutionError> {
    for (stream, chunk) in streams.streams.iter().zip(chunks) {
        prepare_d2h_region(
            &stream.list,
            destination,
            chunk.offset,
            source,
            chunk.offset,
            chunk.bytes,
            None,
        )
        .map_err(|error| level_zero_phase("prepare saturation staged D2H command list", error))?;
    }
    Ok(())
}

fn prepare_h2d_streams(
    streams: &StreamSet<'_>,
    chunks: &[Chunk],
    destination: &ze::DeviceAllocation<'_>,
    source: &ze::HostAllocation<'_>,
) -> std::result::Result<(), CaseExecutionError> {
    for (stream, chunk) in streams.streams.iter().zip(chunks) {
        prepare_h2d_region(
            &stream.list,
            destination,
            chunk.offset,
            source,
            chunk.offset,
            chunk.bytes,
            None,
        )
        .map_err(|error| level_zero_phase("prepare saturation staged H2D command list", error))?;
    }
    Ok(())
}

fn verification_stream<'context>(
    context: &'context ze::Context<'_>,
    device: &DeviceRecord,
    selected: Option<&QueueStreamInfo>,
) -> std::result::Result<QueueStream<'context>, CaseExecutionError> {
    let selected = selected.ok_or_else(|| {
        CaseExecutionError::Skip(format!(
            "dev{} has no copy-capable queue group for verification",
            device.index
        ))
    })?;
    let queue = ze_fatal(
        context.create_command_queue_at(
            &device.device,
            selected.group_ordinal,
            selected.queue_index,
        ),
        "create saturation verification queue",
    )?;
    let list = ze_fatal(
        context.create_command_list(&device.device, selected.group_ordinal),
        "create saturation verification command list",
    )?;
    Ok(QueueStream { queue, list })
}

fn copy_h2d_full(
    stream: &QueueStream<'_>,
    destination: &ze::DeviceAllocation<'_>,
    source: &ze::HostAllocation<'_>,
    bytes: usize,
    phase: &'static str,
) -> std::result::Result<(), CaseExecutionError> {
    ze_fatal(
        copy_h2d_sync(&stream.queue, &stream.list, destination, source, bytes),
        phase,
    )
}

fn copy_d2h_full(
    stream: &QueueStream<'_>,
    destination: &mut ze::HostAllocation<'_>,
    source: &ze::DeviceAllocation<'_>,
    bytes: usize,
    phase: &'static str,
) -> std::result::Result<(), CaseExecutionError> {
    ze_fatal(
        copy_d2h_sync(&stream.queue, &stream.list, destination, source, bytes),
        phase,
    )
}

fn alloc_host<'context>(
    context: &'context ze::Context<'_>,
    bytes: usize,
    phase: &'static str,
) -> std::result::Result<ze::HostAllocation<'context>, CaseExecutionError> {
    ze_fatal(context.alloc_host(bytes, ALLOCATION_ALIGNMENT), phase)
}

fn alloc_device<'context>(
    context: &'context ze::Context<'_>,
    device: &DeviceRecord,
    bytes: usize,
    phase: &'static str,
) -> std::result::Result<ze::DeviceAllocation<'context>, CaseExecutionError> {
    ze_fatal(
        context.alloc_device(
            &device.device,
            bytes,
            ALLOCATION_ALIGNMENT,
            DEVICE_MEMORY_ORDINAL,
        ),
        phase,
    )
}

fn run_warmup(
    options: &BenchOptions,
    events: &mut dyn FnMut(ExecutionEvent),
    mut sample: impl FnMut() -> std::result::Result<Duration, CaseExecutionError>,
) -> std::result::Result<super::sampling::WarmupStats, CaseExecutionError> {
    events(ExecutionEvent::Warmup {
        duration: options.warmup,
    });
    warmup(options.warmup, || sample().map(|_| ()))
}

fn begin_sampling(
    options: &BenchOptions,
    events: &mut dyn FnMut(ExecutionEvent),
    warmup_result: super::sampling::WarmupStats,
) {
    events(ExecutionEvent::Sampling {
        samples: options.samples,
        estimated: estimate_collection(warmup_result, options.samples),
    });
}

fn collect_samples(
    options: &BenchOptions,
    events: &mut dyn FnMut(ExecutionEvent),
    mut sample: impl FnMut() -> std::result::Result<Duration, CaseExecutionError>,
) -> std::result::Result<Vec<Duration>, CaseExecutionError> {
    let mut durations = Vec::with_capacity(sample_capacity(options.samples)?);
    for sample_index in 0..options.samples {
        durations.push(sample()?);
        events(ExecutionEvent::SamplingProgress {
            completed: sample_index + 1,
            total: options.samples,
        });
    }
    Ok(durations)
}

fn collect_observed_samples(
    options: &BenchOptions,
    events: &mut dyn FnMut(ExecutionEvent),
    transfer_class: crate::cli::TransferClass,
    mut sample: impl FnMut(&SampleContext) -> std::result::Result<Duration, CaseExecutionError>,
) -> std::result::Result<Vec<Duration>, CaseExecutionError> {
    let mut durations = Vec::with_capacity(sample_capacity(options.samples)?);
    for sample_index in 0..options.samples {
        let context = sample_context(
            transfer_class,
            options.size_bytes,
            sample_index,
            options.mode,
        );
        durations.push(sample(&context)?);
        events(ExecutionEvent::SamplingProgress {
            completed: sample_index + 1,
            total: options.samples,
        });
    }
    Ok(durations)
}

fn partition_bytes(
    bytes: usize,
    streams: usize,
) -> std::result::Result<Vec<Chunk>, CaseExecutionError> {
    if streams == 0 || bytes < streams {
        return Err(CaseExecutionError::Skip(format!(
            "cannot divide {bytes} bytes across {streams} saturation streams"
        )));
    }
    let base = bytes / streams;
    let remainder = bytes % streams;
    let mut offset = 0;
    Ok((0..streams)
        .map(|index| {
            let chunk = Chunk {
                offset,
                bytes: base + usize::from(index < remainder),
            };
            offset += chunk.bytes;
            chunk
        })
        .collect())
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
            "{path} requires both devices in one Level Zero driver/context"
        )))
    }
}

fn level_zero_phase(phase: &'static str, error: ze::LevelZeroError) -> CaseExecutionError {
    CaseExecutionError::Fatal(BenchmarkError::LevelZeroOperation { phase, error })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partition_is_balanced_disjoint_and_covers_payload() {
        assert_eq!(
            partition_bytes(10, 3).expect("partition"),
            [
                Chunk {
                    offset: 0,
                    bytes: 4
                },
                Chunk {
                    offset: 4,
                    bytes: 3
                },
                Chunk {
                    offset: 7,
                    bytes: 3
                }
            ]
        );
    }

    #[test]
    fn partition_rejects_empty_streams_and_zero_length_chunks() {
        assert!(partition_bytes(8, 0).is_err());
        assert!(partition_bytes(2, 3).is_err());
    }
}
