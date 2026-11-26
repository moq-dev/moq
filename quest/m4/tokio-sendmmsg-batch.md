# [M] Batch cross-connection UDP sends via sendmmsg in moq-sock and moq-tokio

## Goal

`moq_sock::udp::UdpSocketState` exposes a zero-allocation batched send API (`try_send_many` via `sendmmsg` on Linux and Android) with `moq-tokio`'s single-loop I/O driver (built on the runtime-free QUIC core) as its first in-tree caller, flushing each turn's outgoing transmits across connections at the end of the same pass with no extra flush task or wake latency. `moq-uring` is unchanged (its SQE ring already batches sends across connections into one `io_uring_enter`).

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan` follow-up on [moq-dev/moq#9705](https://github.com/moq-dev/moq/issues/9705)):

- ✅ Hold `try_send_many` until [the runtime-free QUIC driver](/quest/m4/quic-sansio-driver.md) lands so `moq-tokio`'s single-loop driver is the in-tree caller under [moq-dev/moq#8957](https://github.com/moq-dev/moq/pull/8957)'s rule ("publish only what something calls"), and design the best long-term batched send API rather than bolting a flush queue onto today's per-connection-task driver (rejected: publishing `try_send_many` in `moq-sock` now with no in-tree caller; adding an opt-in flush-task queue to today's `moq-tokio` driver).
- Moved from `m9/perf` to `m4`: adds a public API to `moq-sock` and requires `quic-sansio-driver` in `m4`.

Evidence and prior art from [#9705](https://github.com/moq-dev/moq/issues/9705):

- In `zshenker/moq@040df9948bd110a83463f35cd4bd86245db03373` (measured in [#7660](https://github.com/moq-dev/moq/pull/7660) and [#8184](https://github.com/moq-dev/moq/pull/8184)), batching today's per-connection-task `moq-tokio` driver through a per-socket queue and flush task raised p50 latency by ~0.2 to 0.3 ms from the flush yield.
- In a single-loop driver (2,000 connections, 4 endpoints, 1,200 B datagrams every 100 ms, `sendmmsg` batches up to 32), flushing each pass directly cut send syscalls from 4,052 to 363 per 50 ms (-3.5% server CPU with stock ACKs), and from 2,189 to 117 per 50 ms (-14% server CPU, 23.1 ms to 19.9 ms) once ACK-only packets stopped dominating, with zero extra flush wait.

Guidance:

- Design the long-term batched send API on `moq_sock::udp::UdpSocketState` (e.g. `try_send_many(&self, socket: UdpSockRef<'_>, transmits: &[Transmit<'_>]) -> io::Result<usize>` on Linux/Android, or the cleanest shape that fits `quic-sansio-driver`'s drain loop and future `quic-io-boundary` buffers):
  - One `sendmmsg` of up to `BATCH_SIZE` (e.g. 32) transmits, running each through `prepare_msg` so every transmit keeps its own destination, GSO `segment_size`, ECN, and `src_ip`.
  - Zero per-call heap allocations.
  - Return the count sent; since `sendmmsg` returns an error only when 0 transmits were sent, errors map cleanly to `transmits[0]`, and `EIO`/`EINVAL` falls back through `send` so the GSO halt and `sendmsg_einval` fallback stay intact.
- Wire it into `moq-tokio`'s single-loop I/O pass once `quic-sansio-driver` lands, draining ready transmits across connections into one batch per turn.
- Verify with a loopback test (many peers, GSO train, ECN, mid-batch oversized transmit pinning which call reports the error), a micro-bench vs `try_send`, and `just bench BASE` on chat/fanout workloads confirming lower CPU/syscalls with no p50/p99 latency regression.

Public API: additive (`moq_sock::udp::UdpSocketState` batched send method). Wire: none.

## Required

- [A runtime-free QUIC driver](/quest/m4/quic-sansio-driver.md) - turns `moq-tokio`'s QUIC driver into a single-loop pass that batches outgoing transmits across connections without a flush task

## Closes

- [#9705](https://github.com/moq-dev/moq/issues/9705) - close once the batched send API and its `moq-tokio` caller land

## Related

- [Fewer ACKs, tail-room piggybacking, and PTO STREAM duplication](/quest/m1/quic-ack-packing-and-pto-duplication.md) - cutting standalone ACKs unlocks the larger -14% CPU win from `sendmmsg`
- [QUIC I/O boundary](/quest/m5/quic-io-boundary.md) - buffer ownership contract for transmit APIs
- [Packet, ACK, and syscall count tracking](/quest/m4/quic-packet-count-metrics.md) - tracks `datagrams / ios` batch ratios
