# xfer Operator Guide

This file is for agents running `xfer` and diagnosing benchmark results. It is
not an implementation specification.

## Operating Rules

- Use a release build for every performance claim.
- Do not start a GPU benchmark while the user or another agent has one running.
- Run pair tests sequentially unless concurrent traffic is the experiment.
- Check active GPU users before attributing low bandwidth to hardware.
- Prefer read-only diagnostics. Do not kill processes, stop the desktop, reset a
  GPU, clear errors, change clocks or power limits, modify PCIe settings, or use
  `sudo` without explicit user approval.
- Record the exact commit, binary, command, runtime, devices, and test settings.
- Never claim a physical P2P route from a Level Zero copy request alone.

## Terms

- **Device**: `devN` is xfer's current enumeration index. Also record the PCI
  address because enumeration order can change.
- **Queue group**: a Level Zero command queue group that advertises capabilities
  such as `copy` or `compute+copy`. A queue group is not proof of a separate
  physical DMA engine.
- **Queue**: an index inside a queue group. Queue numbering restarts in each
  group, so `queue group 0 / queue 0` and `queue group 1 / queue 0` are
  distinct.
- **compute+copy**: the group supports both command types. A copy benchmark
  submits only memory-copy commands; it does not run a compute kernel.
- **Direct**: xfer submits one Level Zero copy command between GPU allocations.
  xfer does not explicitly stage that command through host memory.
- **Explicit staged**: xfer performs D2H, synchronizes, then performs H2D through
  pinned host memory.
- **Peer access**: the result of `zeDeviceCanAccessPeer`, not proof of the
  physical data route.

## Standard Run

From the repository root:

```sh
git rev-parse HEAD
cargo build --release
./target/release/xfer list
xpu-smi ps
./target/release/xfer bench --summary-only
```

The default benchmark is the pairwise ceiling run: every ordered GPU pair,
direct Level Zero copies, saturation across all copy-capable queues, 2 GiB, 50
samples, 1 second of warm-up, and wall-clock timing.

Use detailed output for one suspicious pair:

```sh
./target/release/xfer bench --device 2 --peer-device 0
```

Compare queue groups separately:

```sh
./target/release/xfer bench --single --summary-only
./target/release/xfer bench --device 2 --single --summary-only
```

Run all transfer classes only when the investigation needs them:

```sh
./target/release/xfer bench --class all --summary-only
```

## Read Results

- Use the **median GB/s** as the sustained result.
- Prefer a narrow **p5..p95**, low **MAD**, and few outliers.
- Histogram bar length and the number at its right show how many samples landed
  in that speed bin. A zero is an empty bin, not a missing sample.
- Saturation reports aggregate bandwidth while its selected queues are active
  together. `--single` reports each queue group separately.
- Pair direction matters. `devA -> devB` and `devB -> devA` are separate tests.
- PCIe topology describes where endpoints attach. It does not prove the route
  taken by transfer traffic.
- A pairwise direct-copy median is an isolated directional ceiling. It is not a
  concurrent tensor-parallel collective result. For a conservative TP input,
  use the slowest median among the pairs and directions the collective needs.

## Diagnose A Slow Result

1. Confirm the run used the release binary and the intended payload, mode,
   sample count, and warm-up.
2. Check whether the slowdown follows a source device, destination device,
   device pair, queue group, or time window.
3. Check active GPU processes and live utilization.
4. Compare the affected device's negotiated PCIe link, topology, health, power,
   and frequency with the other devices.
5. Re-run only the affected pair after the system is idle. Do not repeat the
   full sweep unnecessarily.

Useful pattern classification:

- All `devN -> *` directions slow while `* -> devN` stays fast: investigate
  source-side activity, memory pressure, or a source-side path limit.
- Both directions of one pair slow: investigate that pair's topology or shared
  upstream path.
- Only one queue group slow: investigate group-specific scheduling or driver
  behavior.
- A contiguous block of otherwise unrelated cases slow: investigate benchmark
  order and background activity during that time window.
- Both queue groups on one source slow: do not blame one queue group first;
  check device-wide contention.

## Read-Only Diagnostics

```sh
xpu-smi discovery
xpu-smi ps
xpu-smi stats -d 2 -r -j --samples 1
xpu-smi health -d 2 -j
xpu-smi listpciinfo
lspci -tv
```

Replace device `2` with the affected xpu-smi device ID. Map it to xfer using
the PCI address, not only the numeric index.

If Level Zero devices are missing or inconsistent, run the non-destructive
oneAPI/XPU environment probe available to the agent before changing code or
drivers. Stop at the first failed layer: installation, device-node access,
runtime enumeration, then native execution.

## Report Evidence

Include:

- commit and binary path;
- exact xfer command and whether the host was idle;
- host CPU and GPU names with PCI addresses;
- Level Zero/driver and xpu-smi versions when relevant;
- payload, saturation or single mode, samples, warm-up, and timing mode;
- median, p5..p95, MAD, and outlier count;
- direction and queue group;
- peer-access result and reported PCIe topology;
- active GPU processes or utilization that could affect the run;
- facts first, then clearly labeled hypotheses.

Do not describe `direct` as proven physical P2P. State only that xfer submitted
a direct Level Zero GPU-memory copy, whether peer access was supported, and the
PCIe attachment topology it observed.
