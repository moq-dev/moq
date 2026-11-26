# [M] Fix ACK-under-congestion fallback, ECN packet count, ACK_FREQUENCY, MaxAckDelay, and ReceiveTimestamps framing

## Goal

Immediate ACKs are never stalled behind reverse-path congestion, pacing, or `send_rate`; ECN bleaching validation counts newly acknowledged packets rather than range count and ignores non-ECN off-path responses and prior-path ACKs; lost `ACK_FREQUENCY` frames retransmit cleanly without cross-space PN collisions or a one-way delay ratchet; Handshake ACKs never stop 1-RTT `Timer::MaxAckDelay`; `ack.delay` shift never wraps; and `ACK_RECEIVE_TIMESTAMPS` never drops an exact-fit ACK frame, retains already-ACKed timestamps, or lets an unbounded peer limit starve control/STREAM frames.

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan` of the `moq-quic` audit): ✅ plan all verified audit bugs, latency, CPU, and coverage work (rejected: bugs and real-time latency only; drop). Joins `m1` as additive protocol-correctness and real-time latency fixes.

Caused by: [moq-dev/moq#7334](https://github.com/moq-dev/moq/pull/7334) (`ReceiveTimestamps` framing and retention in `rs/moq-quic/src/frame.rs:479, 504`, `rs/moq-quic/src/connection/mod.rs:4108-4119`, `rs/moq-quic/src/connection/receive_timestamps.rs:77-82`); the rest are inherited from upstream `quinn-proto`.

Fixes to land with regression tests:

- **ACK-under-congestion fallback (`connection/mod.rs:815-913`)**: When `SpaceId::Data` has both `can_send.acks` and `ack_eliciting`, and cwnd, `congestion.pace()`, or `send_rate` blocks outgoing data, fall back to emitting an ACK-only packet (`ack_eliciting = false`) instead of skipping the space or breaking out (RFC 9002 §7).
- **ECN packet count & path scoping (`connection/mod.rs:1176-1192, 2070, 3700-3742`)**: Pass the actual newly-acknowledged packet count (not `ArrayRangeSet::len()`, which is the range count) to `process_ecn`, exclude non-ECN off-path `PATH_RESPONSE` packets, and scope ECN validation to `path.generation` across migration.
- **`ACK_FREQUENCY` retransmit, space check, and ratchet (`connection/mod.rs:724-731, 2017`, `ack_frequency.rs:48, 112`)**: Use `|=` (or clear `in_flight_ack_frequency_frame` on loss) so lost `ACK_FREQUENCY` frames aren't overwritten to `false` on the next `transmit()`; guard `ack_frequency.on_acked(packet)` with `space == SpaceId::Data`; compute `candidate_max_ack_delay` from `peer_params.max_ack_delay` rather than the mutated `self.peer_max_ack_delay`; track all in-flight `max_ack_delay` values for `max_ack_delay_for_pto()` (draft-ietf-quic-ack-frequency §7.1).
- **Handshake ACK vs 1-RTT `Timer::MaxAckDelay` (`connection/mod.rs:1215-1219`)**: Stop `Timer::MaxAckDelay` and update `next_bundled_ack_time` only when `space_id == SpaceId::Data`.
- **`ack.delay` shift overflow (`connection/mod.rs:2033`)**: Use saturating shift/multiply so large VarInt `ack.delay` values cannot wrap to 0 and bypass the `peer_max_ack_delay` clamp.
- **`ReceiveTimestamps` framing & bounds (`connection/mod.rs:3861, 4108-4119`, `frame.rs:479, 504`, `receive_timestamps.rs:77-82`)**: Only emit `ACK_RECEIVE_TIMESTAMPS` when `space >= 1` (falling back to plain `ACK` otherwise so `buf.write_var(0)` never pushes `buf.len() > max_size` and drops the whole ACK); prune entries for already-ACKed packets even on partial fits; cap per-ACK timestamp space so an unbounded peer `max_receive_timestamps_per_ack` cannot starve control/STREAM frames.

Public API: none. Wire: none (protocol bug fixes within existing QUIC / draft specs).

## Related

- [Fewer ACKs, tail-room piggybacking, and PTO STREAM duplication](/quest/m1/quic-ack-packing-and-pto-duplication.md) - builds on these ACK and `ACK_FREQUENCY` fixes
- [L4S / AccECN support](/quest/m4/quic-l4s-accecn.md) - builds on fixed ECN packet-count verification
