# Small-frame storage investigation

Measured 2026-09-07 against `1e6c7ce0f`, macOS arm64, Cargo 1.95.0, bytes 1.12.1, release profile. Production code is unchanged. The executable experiment is `rs/moq-net/examples/frame-storage.rs`; raw runs are `bench/frame-storage.csv` and `bench/frame-storage-repeat.csv`.

The promising next step is group-owned payload slabs for small copied/streamed frames, preserving the existing zero-copy path for owned buffers. Packed headers reduce metadata, but the experiment does not establish that they improve live group delivery. Borrowing the group alone does not eliminate payload allocation or reference-count overhead.

## Implementation at the measured revision

- `model/frame.rs`: `Producer<'a>` already exclusively borrows `group::Producer`. `ProducerOwned` serves poll-driven wire code. `Consumer` owns a group-channel handle, not a separate frame channel.
- `model/group.rs`: completed frames live in one `VecDeque<Frame>`. Each record is 48 bytes on this host: 16-byte timestamp plus 32-byte `Bytes`. Whole-frame writes retain an incoming owned buffer without copying. Borrowed inputs allocate their own payload.
- Streaming creates a `FrameBuf` Arc per frame. Two nonempty chunks also allocate a mutable payload buffer. Freezing that buffer calls `Bytes::from_owner`, which boxes an owner. Each partial chunk returned via `FrameBuf::slice` similarly creates a boxed owner. Moving counters into group state would not by itself remove those allocations.
- Rust has no frame-count cap. Its 32 MiB cache budget charges declared payload bytes, not frame records or allocator overhead. JS still has both a 32 MiB payload budget and a 1,024-frame cap (`js/net/src/group.ts`).
- Group reads are sequential, but forward positioning is used by route resumption and range delivery (`model/resume.rs`, `group::Consumer::start_at`). Packed storage can support this by scanning or a sparse index; arbitrary per-frame write positioning is unnecessary.

## Measurements

Nine iterations per case, one warmup, repeated in a second process. Times below are first-run medians, in nanoseconds per frame. Every storage case has 65,536 frames. Production cases use the same count except 1,024-byte payloads, where 16,384 frames avoid cache eviction. Allocation counts are gathered separately from timing and include reallocations. Requested bytes in the CSV are cumulative allocation requests, **not retained memory or peak RSS**.

### Current public API, 64-byte frames

| Write path | Write ns/frame | Read ns/frame | Write allocations/group |
|---|---:|---:|---:|
| `write_frame(Bytes)` | 39.4 | 33.9 | 16 |
| `write_frame(&[u8])` | 40.9 | 41.6 | 65,551 |
| `create_frame`, one whole-buffer write | 124.7 | 33.2 | 65,552 |
| `create_frame`, two 32-byte writes | 160.1 | 32.7 | 196,623 |

Streaming is about 3-4x more expensive to write in this workload. This includes extra group locking, notification, and cache accounting as well as allocation; it is not an allocation-only comparison. Consumers drain completed frames afterward, so this does not measure partial-reader wakeups.

### Storage only, 64-byte borrowed input

| Layout | Write ns/frame | Read ns/frame | Write allocations | First-read allocations |
|---|---:|---:|---:|---:|
| Current `VecDeque<Frame>` shape | 13.7 | 11.1 | 65,551 | 65,536 |
| Same records, 64 KiB payload slabs | 9.4 | 3.6 | 143 | 0 |
| Packed `u32 length, u64 timestamp, payload` | 10.2 | 3.7 | 19 | 1 |

The second run measured 13.7/10.9, 9.0/3.6, and 9.6/3.4 ns respectively. The slab removes nearly all per-frame allocation but retains 48-byte records. The packed layout removes those records too. First-read allocations arise from lazily promoting owned `Bytes` storage to shared storage when cloned; slab sharing already exists at write time.

These are storage-only comparisons: no group mutex, cache policy, timestamps conversion, pending frames, expiry, network, or concurrent readers. Reads create an owned `Bytes` view and expose metadata but do not process every payload byte. Additional untimed verification checks every payload and timestamp. Packed groups are sealed before reads. Slabs use safe `BytesMut::split_to(...).freeze()` and retain a `Bytes` per frame.

Input ownership changes the answer:

- With an already-shared `Bytes`, current records write at 2.9 ns/frame versus 9.3 ns for copying into slabs. Repeated shared input intentionally isolates control overhead and has a much smaller payload working set.
- With a distinct owned allocation per frame, records write at 13.0 ns versus 25.1 ns for slabs. Allocation of that input is included in both cases; copying cannot undo it.
- For borrowed 1-16 byte frames, slabs and packing both improve this experiment. At 256 bytes, packed scanning costs about 16 ns/frame, while dense frame records with slab payloads remain around 3.5 ns. At 1,024 bytes, growing/copying the packed buffer is substantially slower. Header locality and growth copying matter.

## Proposed direction

1. Put the active frame's size, timestamp, written offset, and completion state in the group. Keep the borrowed producer facade. A borrowed consumer facade is possible, but poll-driven drivers still need an owned cursor or state on the group consumer to avoid self-referential state machines. The group producer is cloneable, so an exclusive borrow of one handle does not replace the group-wide open-frame guard. Outstanding readers need their own frame bounds even after the active frame advances. Changing today's owned public consumer to a lifetime-bound type is a published API break and targets `dev`.
2. Prototype small-frame payload allocation from lazily allocated 64 KiB slabs. Preserve owned whole-frame `Bytes` when copying would add work. The measured safe slab implementation is a useful first integration experiment; it leaves record compaction for a separate measured choice.
3. If metadata dominates, compare compact typed descriptors with packed headers. A page-relative offset/length plus group-scale timestamp could use 16 bytes rather than 48, without interleaving hot headers with cold payloads. This layout is a proposal, not measured here. Sparse frame-index anchors can preserve forward positioning without an entry per frame.
4. Keep stable storage for published bytes. A normal growable `Vec` cannot relocate while slices are held. Fixed pages avoid that, bound retention when a consumer holds a tiny slice, and allow whole-page eviction. An unfinished live page still needs a sound publication/ownership scheme; the sealed packed benchmark does not provide one. Avoid a fresh `Bytes::from_owner` for each emitted slice, or the allocation returns on the read side.
5. Account for metadata and allocated page capacity in memory limits, including zero-byte frames. Keep payload traffic stats distinct from memory accounting. Test slow readers and slices that outlive eviction; page references can keep memory alive after the group drops its own reference.

## What “64k” can mean

A 65,536-frame group is practical: the existing records alone cost 3 MiB, before payloads. A 64 KiB *page size* is independent of that frame count and suits slab allocation. A 64 KiB *total group memory budget* cannot hold 65,536 ordinary frames plus headers. This experiment's 12-byte packed header allows at most 5,461 empty frames or 862 frames with 64-byte payloads in 64 KiB, before page bookkeeping.

A payload-only limit does not bound memory: 65,536 one-byte frames need 64 KiB payload plus 3 MiB of current records. Empty frames do not reach a payload budget at all. Under Rust's 32 MiB payload cap, 33,554,432 one-byte frames would require 1.5 GiB of records, excluding payload allocations and capacity slack.

Distinguish cache limits from total stream length. Existing groups can stay open and evict their prefix. A hard 65,536-frame lifetime limit would change that behavior; a retained-frame or memory cap need not.

## Validation and next experiment

Reproduce:

```sh
cargo run --locked --release -p moq-net --example frame-storage
cargo clippy --locked -p moq-net --example frame-storage -- -D warnings
```

Nix was attempted but could not access its cache and daemon socket under the sandbox. The benchmark used direct Cargo; wrapper cache-store warnings did not prevent a successful fresh release build. Two runs completed all count, EOF, timestamp, and payload checks. Clippy passed with warnings denied; rustfmt was applied to the example. Timing is a single-host microbenchmark, not a relay-throughput result.

Before choosing the production layout, integrate the slab path and compare it against the current implementation with small frames arriving in fragments, interleaved producer/consumer polls, multiple readers on different threads, and cache eviction. Measure actual retained memory and allocation counts alongside latency and throughput. Include unfinished-frame drop, abort, partial-frame handoff, empty frames, page boundaries, expiry, and route-resume positioning. Preserve the existing coalesced notification and cache lock ordering. This investigation leaves those implementation and concurrency checks open.

The slab experiment uses the existing bytes dependency. Its documented [split operation](https://docs.rs/bytes/1.12.1/bytes/struct.BytesMut.html#method.split_to) shares backing storage, and [freeze](https://docs.rs/bytes/1.12.1/bytes/struct.BytesMut.html#method.freeze) yields immutable `Bytes` without copying. The [owner constructor](https://docs.rs/bytes/1.12.1/bytes/struct.Bytes.html#method.from_owner) supports retaining custom storage; the pinned source confirms that it boxes each owner.

## Follow-up prototype

See `bench/adaptive-frames.md` for first-frame-sized allocations, capped page growth, a direct `BufMut` writer, arbitrary owned slices, and a separate-versus-packed header comparison with live partial reads. The fixed 64 KiB initial allocation in this first experiment is not the proposed policy for tiny groups.
