# Adaptive allocation in moq-net

This is the first production integration of the storage experiment. It targets `dev` and preserves the public API. It reduces allocation calls for fragmented small frames, but the local measurements do **not** justify a throughput claim or merging it as a speed improvement yet.

## Approach

Keep the existing frame records, contiguous `Bytes` payloads, and kio notification path. Allocate copied chunks from disjoint regions in a group-owned page instead of a separate payload allocation for each frame.

- Allocation is lazy: empty frames and whole-frame owned writes do not allocate a page. `write_frame` and `write_frames` retain their existing behavior, including borrowed input conversion through `IntoBytes`.
- The first page rounds the first fragmented frame's size up to a power of two, with a 128-byte minimum. Later pages double up to 64 KiB, or grow directly to fit the next frame. Frames never straddle pages.
- Frames larger than 64 KiB retain their dedicated allocation, without consuming or growing the small-frame tail.
- Opening a frame transfers the unused tail from the group into its sole writer. Committing returns it under the existing group lock. Allocating or writing into the tail adds no lock or notification.
- Each frame publishes its initialized prefix using the existing Release/Acquire counter. Readers retain the allocation through the frame's owner, so later writes, eviction, and abort cannot invalidate returned slices.
- Finish and cache release discard the group's tail. An active writer owns its tail until it commits or drops. Producer clones share that tail through the group state.

This intentionally keeps the per-frame `Arc` and the `Bytes::from_owner` allocation. Removing those is a separate ownership/storage change. Packing headers would mix metadata and reader changes into the allocation comparison.

## Measurements

Linux x86\_64, AMD Ryzen 7 5800X (8 cores / 16 threads), Rust 1.95.0 (`59807616e`, LLVM 22.1.2), Nix development shell, release builds. Host kernel: `7.0.0-31-generic`. Measured September 7-8, 2026. CPU boost remained enabled; the machine was not exclusively reserved.

The unchanged production baseline is `1329b696d`, the original prototype commit rebased onto `766ff57e9` from `dev`. The after measurements use the production changes accompanying this report, with the same benchmark source, toolchain, and lockfile. Both binaries were built before measuring, with compilation and repository validation stopped during measurement. Runs alternated before/after and then after/before. Each reported run has nine timed repetitions after an allocation-counting warmup.

The table shows the range of the two per-run medians, in nanoseconds per frame. Allocation counts include metadata growth and reallocations, not only payload storage.

| Two-chunk payload | Frames | Before allocations | After allocations | Before write ns/frame | After write ns/frame |
|---|---:|---:|---:|---:|---:|
| 16 B | 65,536 | 196,623 | 131,137 | 248.6-263.3 | 271.6-272.8 |
| 64 B | 65,536 | 196,623 | 131,233 | 248.3-268.0 | 277.4-278.6 |
| 256 B | 65,536 | 196,623 | 131,615 | 254.1-265.0 | 282.5-282.9 |
| 1024 B | 16,384 | 49,165 | 33,305 | 297.1-348.8 | 328.1-340.3 |

For 64-byte frames, allocation calls decrease 33.3%, but write time increases about 7.7% when comparing the average of the two run medians. Reads are similar: 62.2-66.8 ns/frame before and 63.5-65.3 afterward. The shared whole-frame control remains around 66-70 ns/frame to write. This is an allocation reduction with a write-time regression on this host, not a production speedup.

Cumulative requested bytes for that case increase slightly, from 16,252,736 to 16,320,480 bytes. Fewer allocations do not mean less memory: page padding and page-owner metadata remain. The cache still charges logical payload bytes, as before; it is not an RSS bound. Variable frame sizes can leave unused page tails, and retaining a small slice can retain a page of up to 64 KiB. A tiny one-frame fragmented group can pay more allocation/space overhead than the original dedicated buffer.

Raw runs: `adaptive-net-before.csv`, `adaptive-net-before-repeat.csv`, `adaptive-net-after.csv`, and `adaptive-net-after-repeat.csv`. The `model` rows exercise the actual moq-net broadcast/track/group API; the `storage` rows remain isolated control experiments.

The original adaptive prototype also ran twice on Linux at the PR's original `main`-based head, `86690db9f`. Those runs are `adaptive-frames-linux.csv` and `adaptive-frames-linux-repeat.csv`. They are separate from the production comparison because the prototype omits notifications, eviction, and contiguous-frame adaptation. On the first run, separate headers with 64-byte frames and batch-32 publication measured 17.81 ns/frame writing and 45.21 reading; packed headers measured 16.27 and 60.66 respectively.

## Reproduce

Run the same command at the baseline and at the candidate revision, preserving each CSV and recording the revision alongside it:

```sh
nix develop --command cargo run --locked --release -p moq-net --example frame-storage > frame-storage.csv
nix develop --command cargo run --locked --release -p moq-net --example adaptive-frames > adaptive-frames.csv
```

To alternate runs without compiling between samples, copy each built `target/release/examples/frame-storage` binary to a distinct temporary path first. Keep the workload source and lockfile identical for an allocation-only comparison. These examples collect allocations in untimed passes; requested bytes are cumulative allocation requests, not peak or retained memory.

The expanded [A/V-sized matrix](av-frames.md) adds live fanout, larger frames, and retained-storage measurements with fresh processes per case.

## Next decision

Profile the real chunked-frame path before expanding the storage design. Page ownership introduces refcount work and tail transfers that can outweigh a cheap small allocation on this allocator. The next step should demonstrate a win on the production path, including live readers and fanout, before removing per-frame ownership or changing header storage. This PR supplies a concrete implementation and a reproducible counterexample to assuming that fewer allocations automatically means faster delivery.

## Public API and wire

No exported signatures, package versions, framing, timestamps, cache delivery rules, or notification cadence change. JS and IETF draft synchronization is unnecessary for this Rust-private allocation experiment.

## Validation

- `just fix` and `just check` pass in the Nix shell, including native/WASM linting, docs, the transport feature matrix, and flake validation.
- `just test` passes: 3,841 affected-package tests, eight skipped, plus the minimal-relay backend test.
- New tests cover disjoint page reservations, capped growth, dedicated large frames, page lifetime, producer-clone tail sharing, and retained chunks surviving later frames and abort. Existing ingest wake-budget tests pass unchanged.
- Both original release benchmark programs completed, and the unchanged production workload ran twice per revision in alternating order.

The host's Nix requires `NIX_CONFIG='experimental-features = nix-command flakes'` so nested Nix checks inherit feature support. Windows/macOS execution, Miri, and live relay throughput measurements were not run.
