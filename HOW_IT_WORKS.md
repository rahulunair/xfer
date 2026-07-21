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

## Single-queue mode

`xfer bench --single` creates one command queue at queue index 0 for each
selected Level Zero command queue group. One sample records one copy, submits
it, synchronizes its queue, verifies the result, and retains the elapsed
duration.

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

Saturation is the default. `xfer bench --saturation` or `xfer bench -s` states
it explicitly: what aggregate bandwidth is observed when the available
copy-capable queues are kept busy together?

Without `--queue-group`, saturation mode selects every queue index in every
group that advertises Level Zero copy capability. With `--queue-group N`, it
selects every queue index in that group. It does not claim that a queue is a
distinct physical copy engine.

`--size` remains one total logical payload per sample. The payload is divided
into balanced, non-overlapping regions, one per selected queue stream. For
example, the default 2 GiB across four streams transfers 512 MiB per stream,
not 8 GiB total. The requested size must be at least the stream count so every
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
operation, so `--saturation --timing device-timestamps` is rejected.

## Queue group identity

Level Zero exposes command queue groups. Each group has:

- a numeric group ID, called an ordinal by the Level Zero API;
- capability flags such as copy and compute;
- a count of queue indices that can be created in that group.

Human output prints the API object directly, for example
`queue group 1 / queue 2`. Queue numbering starts again inside each group, so
`queue group 0 / queue 0` and `queue group 1 / queue 0` are distinct queues.
CSV uses compact machine-readable field names. These identifiers describe API
topology, not proof of a specific blitter or other physical engine. `xfer`
never silently changes a requested queue group.

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
driver.

The reported PCIe topology is derived from endpoint ancestry in sysfs. It
distinguishes a common root port, a shared upstream bridge, different root
ports, different host bridges, and unknown topology. This attachment
classification is not physical transfer-path proof and does not identify an
upstream bridge as a PCIe switch without stronger evidence.

Destination poisoning and verification use a destination copy-capable queue
group selected independently from the measured source queue group. These
operations are outside the timed interval. A direct case therefore is not
rejected merely because source and destination queue group IDs differ.

`explicit-staged` has a narrower meaning: `xfer` itself issued D2H and H2D legs
through pinned host memory with an explicit barrier between them.

## P2P diagnostics

`xfer diag-p2p --device A --peer-device B` adds platform evidence without
changing the default `bench` behavior. Diagnostic defaults are intentionally
separate from benchmark defaults: 1 GiB, 50 samples, and a 1-second warm-up
unless explicitly overridden with `--size`, `--samples`, or `--warmup`. It runs:

1. An explicit-staged calibration with IIO memory read/write counters.
2. A direct-copy pass with the same memory counters.
3. Repeated direct-copy passes for available peer-write, peer-read, and
   optional UPI counter roles.
4. ACS extended-capability reads on the endpoint bridge ancestry.

ACS is sampled immediately after Level Zero discovery and again after the
measured phases. If bridge evidence changes, the final register values are
reported but ACS does not qualify the verdict. Because `devN` is a Level Zero
enumeration index, xfer cannot read the selected bridge path before opening
Level Zero. An external before/after register read is required to detect a
policy restoration caused by that initial device discovery.

Only measured submit-and-synchronize intervals are observed by the counters.
Allocation, command recording, destination poisoning, and verification remain
outside each counter window. Five idle windows establish a per-phase noise
baseline. Counter decisions use exact integer deltas and reject multiplexed,
zero-running, incomplete, overflowed, or topology-inconsistent evidence.

The default human report is concise and operator-focused: verdict/mechanism,
host-memory counter status, direct versus explicit-staged bandwidth with compact
distributions, route/link context, evidence availability, and actionable
caveats. `--details` reveals raw gate messages, counter role summaries, and ACS
bridge rows. CSV keeps a stable machine-readable summary, including every ACS
bridge outcome and path, for scripts.

A peer counter signal is not enough by itself. A `counter-consistent-peer`
verdict also requires calibrated direct-memory events with valid repeats whose
p90 remains within the idle baseline plus 5% of expected transfer traffic.
Unavailable direct-memory evidence therefore cannot be mistaken for evidence
that host traffic was absent.

The Linux uncore counters are system-wide rather than transaction-tagged to a
Level Zero device pair. Hardware counter restrictions also require separate
repeated passes for memory and peer event sets. For those reasons the strongest
labels are `counter-consistent-peer`,
`counter-consistent-host-memory-traffic`, and
`mixed-signals-across-repeated-runs`, not route proof. Run the command while
other GPU and high-volume I/O activity is idle.

The compiled-in profiles cover Intel SPR-X model `0x8f`, GNR-X model `0xad`,
and GNR-D model `0xae`. Events and map rows are minimal extracts from
`intel/perfmon`; provenance and license details live in
`vendor/intel-perfmon/README.md` and source metadata. The runtime packs source
fields through the PMU format exported by sysfs instead of hard-coding Linux bit
positions.

ACS lives in PCIe extended config space. A short config read is reported as
extended config unavailable, not as no ACS capability. Request Redirect and
Completion Redirect are routing-policy evidence. Egress Control is decoded but
is not called redirect without interpreting an egress vector. No ACS redirect
observation is proof of a direct data path, and an ACS redirect observation is
not proof of host-memory staging.

`diag-p2p` does not auto-elevate. If `perf_event_open` or bridge config reads are
permission-limited, the report preserves that state and prints a same-settings
command using the current executable path for an explicit sudo rerun. Loader
or runtime setup failures must be fixed in the invoking environment; sudo is
not treated as a loader repair mechanism.

## Comparison with ze_peer

Intel's `ze_peer` is useful reference code, but an equivalent comparison
requires matching the operation rather than comparing two labels.

The local Level Zero Tests implementation inspected for this design enumerates
every `(queue-group ID, queue index)` pair and presents a flattened `-u`
"engine" number. Its default flattened index 0 maps to group 0, queue 0.
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
3. Queue group ID and queue index, translating `ze_peer -u` correctly.
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
