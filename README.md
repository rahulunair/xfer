# xefer

`xefer` is a small Unix-style CLI for measuring host/device and
device/device transfer performance on Intel GPUs through the Level Zero API.
It reports application-observed wall-clock time by default and can use Level
Zero device timestamps when the selected device supports them.

The normal workflow needs no hardware-specific arguments:

```sh
xefer bench
```

`xefer` discovers the GPUs and usable copy paths, verifies every copy, and
prints measured bandwidth beside the negotiated PCIe payload ceiling. This is
a low-level transfer baseline for diagnosing slower CCL or model workloads; it
does not claim to measure those higher-level runtimes.

## Requirements

- Rust 1.85 or newer
- Clang and libclang (used by `bindgen`)
- Level Zero headers providing `level_zero/ze_api.h`
- The Level Zero loader library (`libze_loader.so`)
- An Intel GPU with a working Level Zero driver for hardware measurements

On Arch Linux, the development prerequisites are provided by packages such as
`rust`, `clang`, and `level-zero-loader`. Package names differ by distribution.

## Build and test

```sh
cargo build --release
cargo test
cargo clippy --all-targets -- -D warnings
```

The build script generates raw Rust bindings from the installed Level Zero
header. To use a non-standard header location, set `LEVEL_ZERO_INCLUDE` to the
directory containing the `level_zero` directory:

```sh
LEVEL_ZERO_INCLUDE=/opt/level-zero/include cargo build --release
```

## Usage

List GPUs, queue groups, peer-access capability, and negotiated PCIe links:

```sh
xefer list
```

Run the default useful transfer matrix:

```sh
xefer bench
```

Select a transfer and queue explicitly:

```sh
xefer bench --device 0 --class h2d --queue 1 --size 256MiB --samples 50
```

Produce stable machine-readable output:

```sh
xefer bench --format csv --no-histogram
```

Device, queue, transfer-class, size, and sample controls are advanced
overrides. Use `xefer --help` or `xefer bench --help` for the complete
interface.

## Timing and interpretation

Wall-clock samples cover command submission through queue synchronization.
Device-timestamp samples are clearly labeled and are never combined with
wall-clock samples.

Cross-device `direct` means one Level Zero memory-copy command between
allocations owned by different devices. The separately reported
`peer-access=yes|no` value comes from `zeDeviceCanAccessPeer`; it does not prove
that the physical transfer used a peer-to-peer path. `explicit-staged` is one
end-to-end sample containing D2H followed by H2D through pinned host memory,
with synchronization between legs for correctness.

Rates use decimal GB/s (`1 GB = 1e9 bytes`). PCIe percentages use the negotiated
link generation and width read from sysfs. Link data is reported as `unknown`
when the Level Zero device cannot be mapped reliably to a PCI device.
