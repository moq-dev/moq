# [S] Track and guard UDP packet, ACK, PING, and syscall counts across moq-quic, moq-bench, and scale-sim

## Goal

UDP packets sent/received, ACK and PING counts, bytes-per-packet efficiency, and GSO/syscall batch ratios (`datagrams / ios`) are recorded in `moq-bench` and `scale-sim` output and asserted in `moq-quic` sim regression tests (including a zero-PING assertion for active sessions), so packet-count bloat shows up beside CPU and latency regressions.

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan` of the `moq-quic` audit + user packet-count tracking request):

- ✅ Goal confirmed across `moq-quic` sim tests/benches, `moq-bench`, and `scale-sim` (rejected: `moq-quic` tests/benches only; drop).
- ✅ No separate keep-alive quest: native `moq-quic` already resets `Timer::KeepAlive` on every sent/received packet (including ACK-only) and `Timer::Idle` on every received packet, and [keep-alive](https://github.com/moq-dev/moq/pull/9698) owns `{ soft, hard }` keep-alive; add a zero-PING assertion for active one-way and bidirectional sessions here (rejected: also changing QMux's 1 s `QX_PING` probe now).
- Joins `m4` as test, benchmark, and `moq-bench` reporting infrastructure after the 0.22 cut.

Guidance:

- **`moq-quic` sim tests & benches**: Add deterministic sim tests on unidirectional media, bidirectional chat/conferencing, and idle-vs-active sessions that assert upper bounds on `stats.udp_tx.datagrams`, `stats.udp_tx.ios`, `stats.frame_tx.acks`, `stats.frame_tx.ping` (zero PINGs on active sessions), and bytes per datagram.
- **`rs/moq-bench`**: Surface aggregated QUIC transport counters (`udp_tx_datagrams`, `udp_tx_ios`, `udp_rx_datagrams`, `ack_frames_tx`, `ping_frames_tx`) in `rs/moq-bench/src/stats.rs` JSONL output and interval logs (reusing `moq-quic`'s existing `Stats` counters rather than adding new hot-path atomics).
- **`scale-sim`**: Include total packets, ACKs, and `datagrams / ios` per viewer in the audience-scale report alongside instructions, memory, and bytes.

Public API: none (`moq-bench` JSONL adds new additive fields). Wire: none.

## Related

- [Fewer ACKs, tail-room piggybacking, and PTO STREAM duplication](/quest/m1/quic-ack-packing-and-pto-duplication.md) - directly lowers ACK and total packet counts
- [Cross-connection `sendmmsg` batching in `moq-tokio`](/quest/m4/tokio-sendmmsg-batch.md) - directly raises `datagrams / ios`
- [Audience-scale simulation](/quest/m9/scale-sim/README.md) - consumes per-viewer packet counts
