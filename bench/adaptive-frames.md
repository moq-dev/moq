# Adaptive frame storage prototype

The prototype lives in `rs/moq-net/examples/adaptive-frames.rs` and `examples/adaptive_frames/`. It implements both separate headers and packed headers on the same adaptive byte storage. It does not modify the production model or wire format.

## Header choice

Prefer a group-level `Vec<Header>` for the next integration experiment. Fixed-size records grow together; constructing a frame does not allocate a header object. The prototype header is 16 bytes: timestamp ticks and payload length, both `u64`. A group supplies the common timescale. Sequential readers carry their byte position, so an offset is unnecessary. Forward positioning could scan lengths or use sparse anchors later.

Today's `frame::Info` includes a full timestamp with timescale and occupies 24 bytes on this host. It is also a tolerable vector element, but the private storage record can omit the repeated timescale and reconstruct `Info` at the boundary. For 65,536 records, those choices are 1 MiB and 1.5 MiB respectively; today's complete `Frame` records occupy 3 MiB before payload storage.

The packed variant writes the same 16 header bytes into the payload stream. It removes the header-vector allocation, which helps a one-message group, but adds header decoding and can split headers across pages. It does not automatically reduce total memory: those bytes still occupy payload pages, and crossing page boundaries can increase chunk-descriptor capacity. These headers are an internal experimental encoding, not serialized MoQ wire headers.

## Implemented behavior

- Empty groups allocate no payload page. The shared state itself still allocates.
- The first payload allocation rounds the initial frame size to a power of two, with a 128-byte minimum and 64 KiB cap. The packed variant includes its 16-byte header in the hint.
- Subsequent pages double up to 64 KiB. Already-published pages never relocate. Frames may span pages, so a later large frame does not require one oversized page.
- `Writer<'a>: BufMut` exclusively borrows the group. Its writable chunk is bounded by the remaining declared frame size. The benchmark encodes directly with `put_bytes`, without allocating an intermediate frame payload.
- Initialized bytes are published via `BytesMut::split().freeze()`. The unused tail remains mutable and can be written while another thread holds a published prefix.
- `write_owned(Bytes)` accepts arbitrary existing slices without copying. It publishes any preceding direct writes to preserve order. An indexed group using only this path allocates no payload page of its own.
- A group-wide vector holds published chunks, not a vector per frame. Publication can occur after every frame, a batch, a partial-frame flush, or when a page fills.
- Borrowed readers return chunk slices and support partial availability, page crossings, independently cloned group cursors, empty frames, and owned payloads. Finishing a writer commits the frame; group finish publishes the remaining tail.
- An incomplete writer's drop aborts the group. A dropped incomplete reader makes only that cursor unusable. Existing returned byte slices remain valid after abort or group drop.

Example shape inside the prototype:

```rust
let mut writer = group.create(Header { timestamp: 0, size: 100 })?;
writer.put_bytes(42, 100); // Writes directly into the adaptive tail.
writer.finish()?;
group.publish();
```

`finish` and publication are deliberately separate to measure batching. A production convenience API can publish on finish while the ingest driver batches publication. The prototype polls explicitly and has no waker registration. It does not hold a mutex guard while caller code writes into the tail. The only custom unsafe implementation is the bounded `BufMut` adapter; payload allocation, shared ownership, and splitting delegate to `bytes`.

## Measurements

2026-09-07, macOS arm64, direct Cargo release build using the dependency lockfile at `1e6c7ce0f` on `dev`. The PR targets `main`; reruns use that branch's dependency lockfile. Raw results are `bench/adaptive-frames.csv` and `bench/adaptive-frames-repeat.csv`. Each case has a warmup and nine timed repetitions. Tiny groups are repeated within a sample. Allocation counts are gathered separately from timing; they include vector growth and backing-owner allocations. Page and vector capacities are not total RSS and exclude shared-state size, allocator bookkeeping, and caller-owned buffers.

| Case | Header layout | Payload page capacity | Header capacity | Chunk-vector capacity | Write allocations |
|---|---|---:|---:|---:|---:|
| One 100-byte chat frame | Separate | 128 B | 64 B | 128 B | 6 |
| One 100-byte chat frame | Packed | 128 B | 0 B | 128 B | 5 |
| 65,536 empty frames | Separate | 0 B | 1 MiB | 0 B | 17 |
| 65,536 x 64 B, publish each frame | Separate | 4,259,712 B | 1 MiB | 2 MiB | 178 |
| 65,536 x 64 B, publish every 32 | Separate | 4,259,712 B | 1 MiB | 128 KiB | 174 |
| 65,536 x 64 B, publish every 32 | Packed | 5,308,288 B | 0 B | 128 KiB | 191 |

The first chat backing page is 128 bytes, not the entire group's allocation footprint. `Vec` minimum capacities and the shared-state allocation still matter for chat. An inline first header/chunk is a possible follow-up if measurement shows that matters, not implemented here.

With batched publication, the separate-header case emits 2,120 chunks for 65,536 frames; page boundaries account for chunks beyond the 2,048 explicit batch flushes. Publishing every frame emits 65,536 chunks. Thus adaptive allocation removes per-frame payload allocations, but aggressive live publication still retains a 32-byte descriptor per chunk. There is no claim of constant metadata per page for that case.

Wall-clock timing varied substantially between runs and even changed the relative write ranking. For the 64-byte, batch-32 case, separate headers measured 117-158 ns/frame to write and 294-346 ns/frame to read; packed headers measured 156-205 and 676-941 respectively. The straightforward packed reader also takes extra locks and clones a cursor while assembling headers. This is not a controlled demonstration that one physical layout is intrinsically faster. Do not compare these numbers directly against the earlier storage-only benchmark or claim a production relay speedup.

The reproducible findings are correctness, capacity, and allocation counts. Next performance work should interleave paired cases and run under a quieter, controlled environment, then integrate the candidate with the actual group cache and wakeup path. Process inspection was sandbox-denied, so the source of timing variation was not established.

## Validation and limits

```sh
cargo test --locked -p moq-net --example adaptive-frames
cargo run --locked --release -p moq-net --example adaptive-frames
cargo clippy --locked -p moq-net --example adaptive-frames --example frame-storage -- -D warnings
```

Eight tests passed: tiny allocation, geometric growth and page crossings, partial reads and cross-thread production with retained slices, wrong-size abort, empty frames and cloned consumers, arbitrary owned slices, BufMut bounds and dropped writers, mixed direct/owned ordering, and abandoned-reader handling (some cases share a test). Two complete benchmark runs validated frame counts, timestamps, payload contents, EOF, and a zero-copy pointer check. Clippy passed with warnings denied and rustfmt passed. The allocation counter is shared with the earlier experiment under `examples/support/`.

The prototype uses a single writer and explicit polling, not production kio notification, cache expiry/eviction, stats, resumable routing, or group producer clones. It caps declared frame size at 32 MiB but does not implement a total group memory cap. Payloads spanning chunks have a chunked API; a contiguous `Frame.payload: Bytes` adapter would need to copy those frames. Those are integration decisions still to validate, including preserving coalesced wakeups and cache lock order. None of this changes the public API, package versions, or wire behavior.
