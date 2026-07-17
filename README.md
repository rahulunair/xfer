# xfer

`xfer` is a small Unix-style CLI for measuring host/device and
device/device transfer performance on Intel GPUs through the Level Zero API.
It reports application-observed wall-clock time by default and can use Level
Zero device timestamps when the selected device supports them.

The normal workflow needs no hardware-specific arguments:

```sh
xfer bench
```

`xfer` discovers the GPUs and usable copy paths, verifies every copy, and
prints measured bandwidth beside the negotiated PCIe payload ceiling. This is
a low-level transfer baseline for diagnosing slower CCL or model workloads; it
does not claim to measure those higher-level runtimes.

## Requirements

To run a prebuilt `xfer` binary:

- 64-bit x86 Linux with glibc compatible with the release artifact
- The Level Zero loader (`libze_loader.so.1`) and Intel GPU driver
- Permission to access the Intel GPU devices

The binary has no configuration files, daemon, language runtime, or bundled
GPU libraries. Copy the single executable to another machine whose Intel
Level Zero runtime is already working.

To build `xfer`:

- Rust 1.85 or newer
- Clang and libclang (used by `bindgen`)
- Level Zero headers providing `level_zero/ze_api.h`
- The Level Zero loader library (`libze_loader.so`)

On Arch Linux, the development prerequisites are provided by packages such as
`rust`, `clang`, and `level-zero-loader`. Package names differ by distribution.

## Build and test

```sh
cargo build --release
cargo test
cargo clippy --all-targets -- -D warnings
```

The build script generates raw Rust bindings from the installed Level Zero
header and uses `pkg-config` when available. For a non-standard installation,
set `LEVEL_ZERO_INCLUDE` to the directory containing `level_zero` and
`LEVEL_ZERO_LIB` to the loader library directory:

```sh
LEVEL_ZERO_INCLUDE=/opt/level-zero/include \
LEVEL_ZERO_LIB=/opt/level-zero/lib \
cargo build --release
```

## Usage

List GPUs, queue groups, queue counts, peer-access capability, and negotiated
PCIe links:

```sh
xfer list
```

Run the default useful transfer matrix:

```sh
xfer bench
```

Select a transfer and queue group explicitly:

```sh
xfer bench --device 0 --class h2d --queue-group 1 --size 256MiB --samples 50
```

Measure aggregate saturation across all copy-capable queue streams:

```sh
xfer bench --saturation
xfer bench -s --device 0 --class d2d-same-device
xfer bench -s --queue-group 1
```

Produce stable machine-readable output:

```sh
xfer bench --format csv --no-histogram
```

Device, queue-group, transfer-class, size, and sample controls are advanced
overrides. Use `xfer --help` or `xfer bench --help` for the complete
interface.

See [HOW_IT_WORKS.md](HOW_IT_WORKS.md) for the exact timed intervals,
saturation payload accounting, statistical definitions, queue terminology,
and comparison conditions for Intel's `ze_peer`.

## Timing and interpretation

Wall-clock samples cover command submission through queue synchronization.
Device-timestamp samples are clearly labeled and are never combined with
wall-clock samples.

Each case uses a flat sample design: one sample is one independently timed and
verified transfer. Warm-up uses the same command path, while allocation,
initialization, destination clearing, and byte verification stay outside the
measured interval. Increasing iteration counts and fitting a time-per-iteration
regression would hide transfer-to-transfer variation, so `xfer` does not use
that CPU microbenchmark technique.

The main interval is a deterministic 95% percentile-bootstrap confidence
interval for the median measured duration, using 10,000 resamples with
replacement. Bandwidth estimates and interval bounds are derived by inverting
those duration estimates. The report also retains the bandwidth sample p5/p95
spread and unscaled median absolute deviation. Modified Tukey fences at 1.5
and 3 interquartile ranges classify measured durations; outliers are reported
but never discarded. At least 10 samples are required.

Cross-device `direct` means one Level Zero memory-copy command between
allocations owned by different devices. The separately reported
`peer-access=yes|no` value comes from `zeDeviceCanAccessPeer`; it does not prove
that the physical transfer used a peer-to-peer path. `explicit-staged` is one
end-to-end sample containing D2H followed by H2D through pinned host memory,
with synchronization between legs for correctness.

Rates use decimal GB/s (`1 GB = 1e9 bytes`). PCIe percentages use the negotiated
link generation and width read from sysfs. Link data is reported as `unknown`
when the Level Zero device cannot be mapped reliably to a PCI device.
