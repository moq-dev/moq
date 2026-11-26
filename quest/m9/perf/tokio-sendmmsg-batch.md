# [M] Measure and build cross-connection sendmmsg batching in moq-tokio

## Goal

On Linux high-fanout and chat workloads where `moq-tokio` sends 1–2 packets per connection per tick, measure whether batching outgoing UDP transmits across connections via `sendmmsg` (e.g., up to 32 transmits per call) reduces relay CPU and syscall overhead enough to beat the cross-task TX queue hop, and land it in `moq-sock` + `moq-tokio` only if it wins. `moq-uring` is unchanged (its SQE ring already batches sends across connections into one `io_uring_enter`).

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan` of the `moq-quic` audit + user optimization note): ✅ confirm as a measure-first perf quest in `m9/perf` (rejected: dropping it in favor of `moq-uring` alone). Joins `m9/perf` because it touches no published API, wire format, or CLI surface and must prove its cost against the TX channel hop before landing.

Context:

- Today every `moq-tokio` connection task calls `socket.try_send()` -> `libc::sendmsg` independently (`rs/moq-sock/src/udp/unix.rs:294`, `rs/moq-tokio/src/quic/driver/runtime/tokio.rs:51`). Per-connection GSO helps a single bulk stream, not 1,000+ viewers each receiving 1–2 packets per frame tick.
- A downstream user reported `sendmmsg` (32 per call) cutting send syscalls from ~4,050 to 117 per tick (-3.2 ms/tick) in their tick-driven server.
- Unlike a single-threaded tick loop, `moq-tokio` runs one Tokio task per connection, so cross-connection `sendmmsg` needs a shared TX batcher (bounded queue / ring or endpoint TX drain). Keep latency bounded (flush immediately when the batch fills or the queue drains, never add an artificial coalescing timer).

Guidance:

- Add a `sendmmsg` batch send path in `rs/moq-sock/src/udp/unix.rs` (Linux, preserving per-message ECN, `src_ip`, and GSO `segment_size` cmsgs and `EINVAL`/`EIO` fallback).
- Prototype the `moq-tokio` TX batcher and sweep `connections × bitrate` (chat and fanout workloads via `just bench BASE` and `just bench-profile`) measuring syscalls/s, CPU per Gbps, and delivery p99 latency.
- Land if it improves CPU without hurting p99 latency; if the cross-task queue/lock overhead outweighs `sendmmsg` (similar to the channel findings in [quic-lock-contention](/quest/m9/perf/quic-lock-contention.md)), record the numbers and close as a measured no-win.

Public API: none. Wire: none.

## Related

- [QUIC connection lock and packet dispatch profile](/quest/m9/perf/quic-lock-contention.md) - measures cross-task hop costs in `moq-tokio`
- [Packet, ACK, and syscall count tracking](/quest/m4/quic-packet-count-metrics.md) - tracks `datagrams / ios` batch ratios
