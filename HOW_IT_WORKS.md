# How xfer Measures Transfers

`xfer` measures Level Zero memory-copy behavior. It is intended to establish a
reproducible low-level transfer baseline before investigating a slower model,
collective, or framework workload.

## What a result means

The default timing is application-observed wall-clock time. The interval starts
immediately before command submission and ends after every queue used by the
sample has synchronized. Allocation, initialization, command-list recording,
destination poisoning, and byte verification are outside that interval.

Rates use decimal GB/s:

```text
GB/s = logical payload bytes / elapsed seconds / 1,000,000,000
```

Every reported sample is verified. Before a measured sample, the destination is
filled with a sentinel pattern. After synchronization, the complete destination
is copied to host memory when necessary and compared byte-for-byte with the
source pattern. A wrong copy fails the case instead of producing a bandwidth
number.

## Single-transfer mode

`xfer bench` creates one command queue at queue index 0 for each selected Level
Zero engine group. Human output calls this an `engine`; the Level Zero API
calls it a command queue group. One sample records one copy, submits it,
synchronizes its queue, verifies the result, and retains the elapsed duration.

The transfer classes are:

- `h2d`: pinned Level Zero host allocation to device allocation.
- `d2h`: device allocation to pinned Level Zero host allocation.
- `d2d-same-device`: two allocations owned by one device.
- `d2d-direct`: one Level Zero copy between allocations owned by different
  devices.
- `d2d-staged`: D2H followed by H2D through a pinned host allocation.

For explicit staged copies, one sample times both legs end to end. The D2H queue
is synchronized before the H2D leg is submitted because the second leg reads
the host staging buffer.

## Saturation mode

`xfer bench --saturation` or `xfer bench -s` asks a different question: what
aggregate bandwidth is observed when the available copy-capable queues are kept
busy together?

Without `--engine`, saturation mode selects every queue index in every group
that advertises Level Zero copy capability. With `--engine N`, it selects every
queue index in that group. It does not claim that a queue is a distinct
physical copy engine. `--queue-group` remains a compatibility alias.

`--size` remains one total logical payload per sample. The payload is divided
into balanced, non-overlapping regions, one per selected queue stream. For
example, 256 MiB across four streams transfers approximately 64 MiB per stream,
not 1 GiB total. The requested size must be at least the stream count so every
region is non-empty.

Each saturation sample follows this order:

1. Reset and record every command list for its disjoint region.
2. Start the wall-clock timer.
3. Submit every queue stream.
4. Synchronize every queue stream.
5. Stop the timer.
6. Verify the complete destination outside the timed interval.

Submitting all streams before synchronizing any of them is necessary to expose
concurrent queue capacity. Synchronizing each stream immediately after its
submission would serialize the test.

Explicit-staged saturation uses independently selected source and destination
streams. It submits and synchronizes all D2H regions, then submits and
synchronizes all H2D regions inside one timed interval. Its logical payload is
still `--size`, while submitted copy traffic is twice that value. Both values
are shown in text and CSV output.

Saturation supports wall-clock timing only. Independent queues do not provide
one Level Zero device-timestamp interval that represents the aggregate
operation, so `--saturation --device-timestamps` is rejected.

## Engine identity

Level Zero exposes command queue groups. Each group has:

- a numeric group ID, called an ordinal by the Level Zero API;
- capability flags such as copy and compute;
- a count of queue indices that can be created in that group.

Human output calls the group ID an engine ID and prints queue indices in words,
for example `engine 1 / queue 2`. CSV retains API-oriented field names for
compatibility. These identifiers describe API topology, not proof of a
specific blitter or other physical engine. `xfer` never silently changes a
requested engine.

## Warm-up and samples

Warm-up runs the same transfer and synchronization path repeatedly for the
requested duration. It is not included in the reported samples. The progress
display reports the configured warm-up duration and then advances once per
completed, verified sample.

Samples are independent timed observations. `xfer` does not time a large loop
and divide its duration by an iteration count because that would hide
sample-to-sample variation and prevent robust outlier reporting.

## Statistical analysis

Statistics are derived from retained sample durations:

- The center is the median duration.
- The median confidence interval is a deterministic 95% percentile bootstrap
  using 10,000 resamples with replacement.
- The throughput median is `bytes / median duration`.
- Duration confidence bounds are inverted when converted to throughput because
  a longer duration means a lower rate.
- Throughput MAD is the median absolute deviation from the throughput median.
- p5 and p95 use Hyndman-Fan type 7 linear interpolation.
- Quartiles use the same type 7 definition.
- Mild outliers lie outside 1.5 interquartile ranges from Q1 or Q3.
- Severe outliers lie outside 3 interquartile ranges.

Outliers are reported and retained; they are not discarded. A zero MAD means
the samples had no median absolute variation at the available timer resolution,
not that the measurement is exact.

## Timing modes

Wall-clock mode includes application-visible submission and synchronization
cost. This is the default and is the only mode used for saturation and staged
end-to-end results.

Device-timestamp mode uses a Level Zero timestamp event around one copy command.
It measures the device interval supported by that API and is labeled separately.
Wall-clock and device-timestamp samples are never mixed.

## PCIe reference bandwidth

For host/device cases, `xfer` maps the Level Zero device to sysfs and reads the
negotiated PCIe generation and width. The theoretical payload rate accounts for
PCIe line encoding:

- Gen1 and Gen2 use 8b/10b encoding.
- Gen3 through Gen5 use 128b/130b encoding.
- Gen6 and Gen7 use the 242/256 FLIT payload ratio used by this reference
  calculation.

The percentage shown is measured rate divided by this negotiated payload
ceiling. It is not based on product marketing specifications. If the Level Zero
device cannot be mapped reliably to one PCI endpoint, the link is reported as
unknown.

Same-device and cross-device cases do not have one unambiguous PCIe ceiling, so
no synthetic percentage is shown.

## Direct copy and peer access

For every ordered device pair, `xfer` calls `zeDeviceCanAccessPeer`.

`direct` means that `xfer` submitted one Level Zero memory-copy command between
allocations owned by different devices. `peer-access=yes` or `no` is a separate
capability result. Neither observation proves whether the physical path was
device-to-device, traversed a host bridge, or was internally staged by the
driver. A Level Zero trace or platform counter is required to prove the route.

The reported PCIe topology is derived from endpoint ancestry in sysfs. It
distinguishes a common root port, a shared upstream bridge, different root
ports, different host bridges, and unknown topology. This attachment
classification is not physical transfer-path proof and does not identify an
upstream bridge as a PCIe switch without stronger evidence.

Destination poisoning and verification use a destination copy-capable engine
selected independently from the measured source engine. These operations are
outside the timed interval. A direct case therefore is not rejected merely
because source and destination engine IDs differ.

`explicit-staged` has a narrower meaning: `xfer` itself issued D2H and H2D legs
through pinned host memory with an explicit barrier between them.

## Comparison with ze_peer

Intel's `ze_peer` is useful reference code, but an equivalent comparison
requires matching the operation rather than comparing two labels.

The local Level Zero Tests implementation inspected for this design enumerates
every `(queue-group ordinal, queue index)` pair and presents a flattened
`-u` engine number. Its default flattened index 0 maps to group 0, queue 0.
Its `--parallel_single_target` path divides one total buffer among selected
flattened queue entries, submits the selected command lists, then synchronizes
them. That payload interpretation aligns with `xfer --saturation`.

The defaults are not equivalent: `xfer` saturation selects groups advertising
copy capability, while `ze_peer` documents compute-capable entries as its
parallel default. Select and map queues explicitly for a comparison.

Important differences remain:

- `ze_peer` normally times a loop of iterations and reports total time divided
  by the iteration count. `xfer` retains independent sample durations and
  reports robust summaries and a confidence interval.
- `ze_peer` warm-up defaults to an iteration count. `xfer` warm-up is
  duration-based.
- `ze_peer` can use immediate command lists and several bidirectional or
  multi-target modes that are not the same operation as `xfer` saturation.
- `xfer` poisons and verifies every measured destination. `ze_peer` validation
  behavior depends on its options and path.

Before comparing numbers, match all of the following:

1. Source and destination devices and transfer direction.
2. Total logical byte count.
3. Queue-group ordinal and queue index, translating `ze_peer -u` correctly.
4. Number of concurrent streams.
5. Regular versus immediate command-list mode.
6. Unidirectional versus bidirectional traffic accounting.
7. Host allocation type and direct versus explicit-staged path.
8. Wall-clock interval boundaries and validation settings.

`ze_peer` refuses pairs for which `zeDeviceCanAccessPeer` reports false.
Therefore, compare a `ze_peer` P2P result only with an xfer
`direct, peer-access=yes` case. An xfer `direct, peer-access=no` result remains
a valid observation of a Level Zero copy accepted by that driver, but there is
no equivalent `ze_peer` P2P measurement for that pair.

Even after matching these controls, small differences are expected because the
sampling and summary estimators differ.

## What this does not measure

These results are not oneCCL collective bandwidth, tensor-parallel throughput,
model tokens per second, or end-to-end training/inference performance. Those
workloads add synchronization, kernels, topology-aware routing, protocol
thresholds, and framework overhead. `xfer` supplies a lower-level transfer
baseline that helps determine whether the transport foundation is already
underperforming. Pair cases run one at a time, so their direct medians are
isolated directional pair ceilings, not concurrent multi-GPU collective
rooflines.
