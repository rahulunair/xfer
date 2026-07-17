# xfer

`xfer` is a small Rust CLI for measuring Intel GPU transfer bandwidth through
the Level Zero API. It verifies every copy and clearly separates:

- direct device-to-device copy requests;
- explicit copies staged through pinned host memory;
- Level Zero peer-access permission;
- PCIe attachment topology.

## Install

Prebuilt releases support Linux x86_64 and require the Intel Level Zero runtime
(`libze_loader.so.1`).

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://raw.githubusercontent.com/rahulunair/xfer/master/install.sh | sh
```

The binary is installed to `~/.local/bin/xfer`.

## Use

```sh
xfer list
xfer bench
```

`xfer bench` measures every ordered GPU pair sequentially using direct Level
Zero copies, all copy-capable queues, a 2 GiB payload, 50 samples, and a
1-second warm-up. `--saturation` is accepted as an explicit form of the default.

## Common Commands

| Task | Command |
| --- | --- |
| Inspect GPUs, engines, peer access, and PCIe topology | `xfer list` |
| Direct-copy ceiling for every GPU pair | `xfer bench` |
| Compact pairwise roofline report | `xfer bench --summary-only` |
| Detailed result for one pair | `xfer bench --device 0 --peer-device 1` |
| Full transfer matrix, compact report | `xfer bench --class all --summary-only` |
| Stable machine-readable output | `xfer bench --format csv` |

Run `xfer bench --help` for filters and output options.

## Example Result

```console
$ xfer bench
System under test
  Host  Intel(R) Xeon(R) 676X
        1 socket, 32 cores, 64 threads
  dev0  Intel(R) Arc(TM) Pro B70 Graphics
        PCI 0000:0d:00.0 | Gen5 x16 | 63 GB/s theoretical
        engines 0 compute+copy; 1 copy
  dev1  Intel(R) Arc(TM) Pro B70 Graphics
        PCI 0000:64:00.0 | Gen5 x16 | 63 GB/s theoretical
        engines 0 compute+copy; 1 copy
  dev2  Intel(R) Arc(TM) Pro B70 Graphics
        PCI 0000:90:00.0 | Gen5 x16 | 63 GB/s theoretical
        engines 0 compute+copy; 1 copy
  dev3  Intel(R) Arc(TM) Pro B70 Graphics
        PCI 0000:a6:00.0 | Gen5 x16 | 63 GB/s theoretical
        engines 0 compute+copy; 1 copy

D2D direct dev0 -> dev1
  Transfer
    payload             2 GiB
    mode                saturation across 2 queues; payload partitioned across queues
    queues              engine 0 / queue 0 (compute+copy); engine 1 / queue 0 (copy)
    memory              device memory
    timing              wall clock, 50 samples, 1 s warm-up

  P2P evidence
    copy request        direct GPU-memory copy (Level Zero)
    peer access         supported (zeDeviceCanAccessPeer = yes)
    PCIe topology       different host bridges pci0000:0a -> pci0000:61
    host staging        none requested by xfer

                lower        median       upper
  time        [ 48.641 ms    48.646 ms    48.651 ms   ]
  throughput  [ 44.14 GB/s   44.145 GB/s  44.149 GB/s ]
                95% bootstrap confidence interval (10000 resamples)
  sample       p5 44.1 GB/s, p95 44.17 GB/s
  variability  MAD 0.01 GB/s
  outliers     4/50 (2 mild, 2 severe)

  distribution  GB/s (50 samples)
    44.03 │ █                         1
    44.05 │                           0
    44.07 │ █                         1
    44.09 │ ███                       3
    44.11 │ ████████████             12
    44.14 │ ████████████████████████ 24  ◆ median
    44.16 │ ████████                  8
    44.18 │                           0
    44.20 │                           0
    44.23 │                           0
    44.25 │                           0
    44.27 │ █                         1
```

Use the median as the sustained pairwise bandwidth input. Short bursts in the
upper tail are not a stable roofline.

## Interpretation

- `direct` means one Level Zero copy request between allocations on different
  GPUs. `xfer` does not insert a host stage into that timed path.
- `explicit-staged` measures D2H, a synchronization point, then H2D through
  pinned host memory. It reports logical payload rate and 2x copy traffic.
- `peer access: supported` is the Level Zero P2P capability result. It does not
  prove the physical DMA route.
- PCIe topology labels come from endpoint ancestry in sysfs.
- Pair tests are isolated directional ceilings, not concurrent tensor-parallel
  collective benchmarks.

Rates use decimal GB/s. Wall-clock timing is the default.

See [HOW_IT_WORKS.md](HOW_IT_WORKS.md) for measurement boundaries, statistics,
saturation semantics, and comparison notes.

## Build

Building requires Rust 1.85+, Clang/libclang, Level Zero headers, and the Level
Zero loader.

```sh
cargo build --release
cargo fmt --all -- --check
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
```
