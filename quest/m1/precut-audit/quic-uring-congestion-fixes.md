# [M] Fix moq-quic varint/assembler/datagram bounds, moq-uring CONNECT leftovers, and congestion guards

## Goal

`moq-quic` never panics on `MAX_STREAM_DATA`/`MAX_STREAMS` varint bounds or
suffers quadratic `Assembler` defragmentation, `Datagrams::max_size` respects
disabled receive buffers, `moq-uring` preserves bytes following WebTransport
CONNECT HEADERS, and `moq-congestion` handles zero-RTT / zero-delay safely.

## Plan

Decided 2026-11-21 (maintainer, /quest-plan of the exhaustive pre-cut
full-codebase audit): ✅ place high-severity and impactful protocol/correctness
bugs in `m1` (rejected: `m0` with `release` backports; `m3`).

1. **`rs/moq-quic/src/connection/streams/{recv.rs:294,state.rs:241-246,592-616}`**:
   - Clamp emitted `MAX_STREAM_DATA` values to `VarInt::MAX` so `write_var`
     cannot panic when a large window is added to a near-max offset.
   - Clamp `MAX_STREAMS` to `MAX_STREAM_COUNT` (`2^60`) per RFC 9000.
2. **`rs/moq-quic/src/connection/mux/mod.rs:661` & `assembler.rs:152,160`**:
   - Drop `.max(limit as usize)` from `Assembler::allocated` (which inflates
     the reported allocation, breaks O(n²) defrag thresholds, and can overflow
     `usize` on large limits) and use saturating arithmetic.
3. **`rs/moq-quic/src/connection/datagrams.rs:73-82`**:
   - Return `None` from `Datagrams::max_size()` when `receive_buffer.is_none()`
     rather than advertising datagram capacity when receive buffering is off.
4. **`rs/moq-uring/src/quic/web.rs:1208-1221` (callers `:220,478`)**:
   - Preserve any trailing bytes already buffered past the HTTP/3 WebTransport
     CONNECT `HEADERS` frame and pass them into `read_capsules` so coalesced
     capsule bytes are never silently dropped.
5. **`rs/moq-congestion/src/pacer.rs:140-147` & `cubic.rs:68,147-169`**:
   - Clamp non-zero pacing delay to `>= 1ns` in `pacer.rs` to prevent zero-step
     spin/divide issues, and guard `rtt.is_zero()` in `cubic.rs`.

Public API: none. Wire: RFC 9000 / HTTP/3 capsule conformance fixes. Add unit
tests for each of the five fixes across `moq-quic`, `moq-uring`, and
`moq-congestion`.
