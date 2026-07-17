# xfer

`xfer` is a small Rust CLI for measuring Intel GPU transfer bandwidth through
the Level Zero API. It verifies every copy and clearly separates:

- direct device-to-device copy requests;
- explicit copies staged through pinned host memory;
- Level Zero peer-access permission;
- PCIe attachment topology.

## Quick Start

Build requirements: Rust 1.85+, Clang/libclang, Level Zero headers, and the
Level Zero loader.

```sh
cargo build --release
./target/release/xfer list
```

Get one sustained direct-copy row for every ordered GPU pair, using all
copy-capable queues:

```sh
./target/release/xfer bench --class d2d-direct --saturation --summary-only
```

Pairs run sequentially.

## Common Commands

| Task | Command |
| --- | --- |
| Inspect GPUs, engines, peer access, and PCIe topology | `xfer list` |
| Direct-copy ceiling for every GPU pair | `xfer bench --class d2d-direct --saturation --summary-only` |
| Detailed result for one pair | `xfer bench --device 0 --peer-device 1 --class d2d-direct --saturation` |
| Full transfer matrix, compact report | `xfer bench --summary-only` |
| Stable machine-readable output | `xfer bench --format csv` |

Run `xfer bench --help` for advanced filters.

## Example Result

```text
System under test
  Host  Intel(R) Xeon(R) 676X
        1 socket, 32 cores, 64 threads
  dev0  Intel(R) Arc(TM) Pro B70 Graphics
        PCI 0000:0d:00.0 | Gen5 x16 | 63 GB/s theoretical
        engines 0 compute+copy; 1 copy
  dev1  Intel(R) Arc(TM) Pro B70 Graphics
        PCI 0000:64:00.0 | Gen5 x16 | 63 GB/s theoretical
        engines 0 compute+copy; 1 copy

D2D direct dev0 -> dev1
  Transfer
    payload             256 MiB
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
  time        [ 13.779 ms    13.782 ms    13.783 ms   ]
  throughput  [ 19.475 GB/s  19.477 GB/s  19.482 GB/s ]
                95% bootstrap confidence interval (10000 resamples)
  sample       p5 19.46 GB/s, p95 26.48 GB/s
  variability  MAD 0.01 GB/s
  outliers     9/50 (1 mild, 8 severe)

  distribution  GB/s (50 samples)
    19.4 | ########################  median
    20.2 |
    21.0 |
    21.8 | #
    22.6 |
    23.3 |
    24.1 |
    24.9 |
    25.7 | ##
    26.5 | ##
    27.2 |
    28.0 | #
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

## Validate

```sh
cargo fmt --all -- --check
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
```
