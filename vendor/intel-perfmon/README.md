# Intel perfmon Extracts

Minimal source extracts from `intel/perfmon` are vendored for xfer hardware-counter evidence generation.

- Upstream: `https://github.com/intel/perfmon`
- Commit: `6e3329d20457aad11d8cc323b85aa6a16b075918`
- License: BSD-3-Clause, preserved in `LICENSE`
- Runtime dependency: none

Check the vendored extracts and generated Rust constants with:

```sh
python3 tools/update_perfmon_events.py --check
```

To refresh from a local checkout of the exact pinned commit:

```sh
python3 tools/update_perfmon_events.py --import-from /path/to/intel-perfmon
python3 tools/update_perfmon_events.py --check
```

The importer rejects a different commit or source hash, rewrites only the
selected source records and map rows, and regenerates the Rust constants.

The extracted events are observations for later diagnostics. They are not physical-route proof.
