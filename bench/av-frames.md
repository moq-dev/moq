# A/V-sized frame benchmarks

The `av-frames` example exercises moq-net's production group/frame API. The matrix has 180 cases: ten fixed sizes and two mixed profiles, five input paths, and three reader patterns. This extends the small-frame experiment without changing library behavior.

## Workloads

- Fixed payload sizes: 100 B, 256 B, 1,000 B, 4 KiB, 16 KiB, 32 KiB, 64 KiB, 64 KiB + 1 B, 256 KiB, and 1 MiB. Groups contain 60 frames, reduced to keep payloads within 8 MiB (32 frames at 256 KiB; eight at 1 MiB).
- Mixed audio: 50 frames cycling through 100, 240, 400, 160, and 1,000 bytes.
- Mixed video: 60 frames, starting with a 256 KiB keyframe-sized payload, followed by a repeating 4/16/32 KiB pattern.
- Inputs: `Owned` calls `write_frame` with existing `Bytes`; `StreamOwned` creates a frame and writes an owned buffer; `Split` forces two borrowed chunks; `Chunk1200` and `Chunk16k` cap each borrowed write at that many bytes. A cap larger than the frame produces a single write, so it does not exercise fragmented storage.
- Readers: `Backlog` drains a completed group with one reader; `Live` has one reader consume after each write; `Fanout` does the same with eight readers. Live readers register persistent kio waiters, and the harness refuses to advance an unnotified reader.

These are synthetic byte distributions, not captured Opus/H.264/AV1 traces. The model treats payloads as opaque bytes. The live cases run cooperatively on one thread, with no network, pacing, cross-thread scheduling, or real-time latency measurement. Timestamps verify ordering and propagation, not a simulated playback rate.

## Method

Each case runs in a fresh process to avoid allocator history from unrelated cases. The runner builds the exact same harness against baseline `1329b696d` and candidate library `45051921d`, checks that their lockfiles match, and uses separate Cargo target directories. Sharing the target directory can leave the un-hashed executable from the other checkout even when Cargo reports a build fresh.

Both binaries are built before timing. For each case, the order is before/after/after/before. Each process verifies one warmup group, then takes nine timed samples; samples repeat groups up to 2,048 frames or 8 MiB of payload, with a 64-group cap. Inputs are allocated and their `Bytes` ownership is prepared outside timing. The timer includes group/track/broadcast construction, writing, reading, notification handling, and teardown. `ns_per_frame` is per produced frame, including all readers. It is not directly comparable to the earlier write-only benchmark.

An additional untimed group checks every byte, timestamp, frame count, EOF, and live notification. Allocation counters are enabled only for that pass; the allocator wrapper remains installed during timing with counting disabled. Their counts and memory snapshots repeated exactly across both runs of all 180 cases. Each allocation-counting pass also verifies that dropping the retained slice returns the tracked live allocation balance to zero.

The memory columns count requested heap bytes, including model, reader, and ownership metadata. They exclude prebuilt input buffers, allocator bookkeeping/rounding, and RSS. `peak_live_bytes` is the peak after allocator calls complete (not transient memory inside `realloc`); `cached_live_bytes` is sampled after group finish and before the final drain. `retained_live_bytes` is sampled after all model handles are dropped while a reader holds up to 64 bytes from the first chunk of the **last** frame (50 bytes for a split 100-byte frame). This exposes retained page storage without adding a public inspection API. `requested_bytes` is cumulative allocation traffic; it is not live memory. Instrumentation is single-threaded and must not be used for a cross-thread allocation experiment.

## Results

Linux x86\_64, Ryzen 7 5800X, Rust 1.95.0 / LLVM 22.1.2, Nix shell, September 8, 2026 UTC. CPU boost was enabled; the host was not exclusively reserved. Raw measurements are `av-frames-{before,after}-{1,2}.csv`.

The table shows the range of the two run medians for **two-chunk writes with one live reader**, in microseconds per produced frame. Allocation and retained-byte columns show before / after; allocations include setup and reading for one group.

| Payload bytes | Frames/group | Allocations before / after | Before us/frame | After us/frame | Retained bytes before / after |
|---|---:|---:|---:|---:|---:|
| 100 | 60 | 341 / 293 | 0.65-0.66 | 0.67-0.67 | 188 / 4,216 |
| 256 | 60 | 341 / 293 | 0.74-0.74 | 0.74-0.75 | 344 / 8,312 |
| 1,000 | 60 | 341 / 293 | 0.74-0.75 | 0.76-0.76 | 1,088 / 32,888 |
| 4,096 | 60 | 341 / 295 | 0.76-0.87 | 1.47-1.49 | 4,184 / 65,656 |
| 16,384 | 60 | 341 / 315 | 3.05-3.10 | 1.36-1.40 | 16,472 / 65,656 |
| 32,768 | 60 | 341 / 343 | 3.30-4.73 | 12.25-12.82 | 32,856 / 65,656 |
| 65,536 | 60 | 341 / 401 | 4.73-5.77 | 22.53-22.57 | 65,624 / 65,656 |
| 65,537 | 60 | 341 / 341 | 4.75-5.88 | 4.72-4.79 | 65,625 / 65,625 |
| 262,144 | 32 | 200 / 200 | 12.17-12.52 | 12.06-12.19 | 262,232 / 262,232 |
| 1,048,576 | 8 | 78 / 78 | 204.88-205.65 | 213.43-217.47 | 1,048,664 / 1,048,664 |

The larger sweep argues against merging the current allocator as a general A/V optimization:

- Audio-sized fragmented groups reduce allocation calls, but these live-reader timings show little benefit. Holding a 64-byte slice of a 256-byte frame retains 8,312 bytes afterward versus 344 before. At 1,000 bytes, that becomes 32,888 versus 1,088 bytes.
- Pooling stops saving allocation calls around 32 KiB and adds an allocation per frame at exactly 64 KiB. Each frame occupies a whole page there, so the shared page owner provides no amortization. Above 64 KiB, allocation counts and retained storage return to the dedicated-buffer baseline.
- Video timing depends strongly on both size and reader pattern. At 64 KiB, the single-reader split case is much slower after pooling, while the eight-reader split case is much faster (before 26.32-26.36 us/frame; after 4.77-5.31). The counts are deterministic, but this inversion needs allocator/CPU profiling before attributing a general speedup to the storage design.
- The mixed-video split workload reduces allocations from 341 to 324 with a live reader, but increases requested live heap at group finish from 1,337,404 to 1,571,572 bytes and doubles the storage retained by a slice of its final 32 KiB frame. Owned whole-frame inputs remain a separate zero-copy control.

Keep the owned-buffer path. Profile representative ingest and allocator behavior before retaining this pooling policy; a cutoff below the page size would avoid the obvious per-frame page-owner penalty, but would not by itself solve small-slice retention.

## Run

Inside the Nix shell, run the full paired matrix with an empty output directory:

```sh
nix develop --command bash bench/av-frames.sh 1329b696d /tmp/moq-av-comparison
```

The runner writes four CSVs, source snapshots, revision/host/toolchain metadata, and binary hashes. It builds temporary baseline and candidate targets and removes them on exit. It refuses mismatched lockfiles and existing output CSVs.

To list or inspect one case:

```sh
nix develop --command cargo run --locked --release -p moq-net --example av-frames -- --list
nix develop --command cargo run --locked --release -p moq-net --example av-frames -- --case fixed-65536/Split/Live
```

Running without a filter is useful for correctness, but use the per-case runner for comparisons so earlier workloads cannot change allocator state. On hosts where Nix features are disabled, prefix the commands with `NIX_CONFIG='experimental-features = nix-command flakes'`.

No public API, wire format, or package versions change. The added tests cover the input/reader matrix, retained-slice lifetime, and allocation/reallocation/deallocation accounting.

## Validation

`just fix 45051921d`, `just check 45051921d`, and `just test default 45051921d` pass. The affected-package suite ran 3,843 tests successfully, with eight skipped, followed by the passing minimal-relay test. The paired runner completed 720 isolated case invocations; all repeated allocation, live-memory, retained-memory, and wake counts matched exactly.
