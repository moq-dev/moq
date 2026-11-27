# [S] Batch frames and cut hot-path allocations in js/qmux

## Goal

`js/qmux` packs multiple frames per QMux record on `qmux-01/02`, removes per-frame send/receive allocations and redundant copies, replaces O(N) scheduling and datagram dequeue operations with O(1) structures, and drops the redundant `TransformStream` wrapper on native `WebSocketStream`, verified with before/after benchmarks in `js/qmux/bench/` and no wire or public API breaks.

Boundaries and non-goals: pure TypeScript optimizations inside `js/qmux`; no wire breaks, no WASM compilation of `js/qmux`.

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan` of the QMux audit): ✅ Confirm full scope including `js/qmux` perf (rejected: Correctness and web-transport ports only; Correctness and consolidation, skip perf; Drop). Joins `m4` (behind the release cut) as optimization work with no blocking release consumer.

Work items (report before/after `js/qmux/bench/` numbers):

1. **Multi-frame record batching (`scheduler.ts:383-433`, `frame.ts`)**: Pack queued control (`MAX_DATA`, `MAX_STREAM_DATA`, `FIN`) and small `STREAM` frames into one QMux record up to the peer's `max_record_size` on `qmux-01/02`, avoiding a separate WebSocket frame, 4-byte browser mask, and TLS record per frame.
2. **Cut per-frame allocations and copies (`frame.ts:305-493, 698`)**: Reuse a scratch encode buffer instead of allocating a fresh `ArrayBuffer` + 4 `DataView`s per `STREAM` frame, and skip unconditional `own()` payload copies on inbound frames when a slice does not pin a large backing buffer.
3. **O(1) scheduling and datagrams (`scheduler.ts`)**: Replace the O(N) ready-stream scan in `#pickStream` and `Array.shift()` on `#datagrams` with O(1) structures (priority bucket/heap or queue + ring buffer).
4. **Drop redundant native `WebSocketStream` receive `TransformStream`**: Remove the `event.readable.pipeThrough(new TransformStream(...))` wrapper in `web-socket-stream.ts:365-371` by using an internal transport type for `Session` (keeping exported `WebSocketStreamLike` / `WebSocketStreamOpenEvent` types unchanged) and relying on the `ArrayBuffer` normalization already performed in `js/qmux/src/session.ts` (noted in `audit-perf.md:113`).

Public API: none. Wire: none (multi-frame records are standard `qmux-01/02`).

## Required

- [Fix Rust and JS QMux correctness bugs and port web-transport fixes](/quest/m1/qmux-correctness-fixes.md) - lands the JS wire and state-machine fixes before optimizing the hot path
