# [M] Eliminate per-packet and per-ACK hot-path allocations and copies in moq-quic

## Goal

Measured instruction counts and heap allocations per packet/frame drop across `moq-quic`'s TX, ACK, receive-lease, crypto, and QMux hot paths, keeping only changes that show a measured win on `moq-quic`'s benches and `just bench-runtime`.

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan` of the `moq-quic` audit): ✅ plan all verified audit bugs, latency, CPU, and coverage work (rejected: bugs and real-time latency only; drop). Joins `m9/perf` because every change is internal with no public API, wire, or CLI impact and should be gated by before/after instruction and allocation measurements.

Audit sites to measure and fix:

- **`Schedule` 1-stream fast path & `QUANTUM` (`streams/schedule.rs:31, 70-99, 488-523`)**: With `send_fairness = true`, `Queue::pop()` + `Schedule::sent()` remove and re-insert into `BTreeMap`, triggering a `1 -> 0 -> 1` ~400 B root-node malloc/free on **every packet** when a group has 1 active stream (the common MoQ case). Keep a 1-item inline slot before spilling into `BTreeMap`, and raise `QUANTUM` to at least the path MTU (currently 1200 B < 1452 B Ethernet PMTU, causing extra `pop_next` iterations every packet).
- **`SendBuffer::ack` in-order fast path & `VecDeque` thrashing (`send_buffer.rs:85-106`, `btree_range_set.rs:62-86, 201-205`)**: Fast-path `range.start == base_offset` when `self.acks.is_empty()` to avoid a `BTreeMap` `0 -> 1 -> 0` root alloc/free on every in-order ACKed STREAM frame; move `unacked_segments.shrink_to_fit()` out of the `while` loop with a small minimum capacity floor (e.g., `capacity > 8`) so steady streams don't alloc/free every write/ACK cycle.
- **Spurious `poll_lease` wakeups & copy amplification (`streams/state.rs:335-342`, `streams/lease.rs:134-189`)**: Track contiguous readable bytes in `Assembler` so `StreamsState::received` wakes `Wait::Read` / calls `lease.arrived()` only when `read()` has bytes or `lease.len + contiguous >= want` (or FIN/reset) rather than on every packet; eliminate the per-call temporary `Vec<Bytes>` in `Leased::fill()`.
- **Double-boxed `rustls` crypto trait objects (`crypto/rustls.rs:121-139, 389-409`)**: Remove the double `Box<Box<dyn ...>>` indirection on `PacketKey` and `HeaderKey` so every packet encrypt/decrypt/header-protect/`tag_len` pays one indirection instead of two.
- **ACK & loss hot-path allocations (`connection/mod.rs:1950, 1983-2025, 2091, 2325, 2398`, `frame.rs:476-477`)**: Drain ACK ranges directly from `Slots` chunks instead of inserting descending PNs into `ArrayRangeSet` one by one; reuse scratch `Vec`s on `Connection` for loss, spurious-loss, and `ReceiveTimestamps` encode/decode; use stack `[u8; 32]` in `ResetToken::new` (`token.rs:361`).
- **Control-frame drain & smaller hot-path wins**: Replace `FxHashSet::iter().next()` + `remove()` (`O(C * N)` bucket rescans in `streams/state.rs:799-803, 883-887`) with `IndexSet::pop()`; avoid dropping `pending` `Retransmits` capacities on every send (`spaces.rs:177`); hash only `&self.bytes[..len]` in `ConnectionId` (`shared.rs:80-86`); iterate set bits of `chunk.mask` in `Slots::values_mut` / `into_values` (`slots.rs:139-150`); stop overcharging `allocation >= 16 KiB` per small QMux STREAM frame (`mux/mod.rs:684`) so QMux stops defragmenting every 3 frames.

Measure before and after with instruction counts on `rs/moq-quic/benches/` and `just bench-runtime`. Drop any sub-item that shows no measurable win.

Public API: none. Wire: none.

## Related

- [Relay allocations](/quest/m9/perf/relay-allocs.md) - end-to-end fanout relay heap profile
- [QUIC I/O boundary](/quest/m5/quic-io-boundary.md) - zero-copy RX/TX seam across `moq-uring`
