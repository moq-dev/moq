# [M] Reduce ACK packet count with configurable max_ack_delay, tail-room piggybacking, and PTO STREAM duplication

## Goal

`moq-quic` emits far fewer standalone ACK packets without regressing tail-loss latency: `max_ack_delay` (default 50 ms) and `ack_eliciting_threshold` (default 1) are configurable without the 25 ms clamp, ACKs reserve tail room to piggyback onto outgoing `STREAM` packets once `max_ack_delay / 2` has elapsed (sending a dedicated ACK at `max_ack_delay`, and immediately on loss or ECN CE), `SendBuffer::fit` uses the exact length varint size instead of subtracting 8 bytes, ACK delay uses `self.arrival` rather than loop `now`, and PTO probes duplicate the oldest in-flight `STREAM` data instead of falling back to `PING`.

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan` of the `moq-quic` audit + packet-count optimization):

- ✅ Goal confirmed: cut UDP packet and RX/TX CPU cost via configurable `max_ack_delay` / `ack_eliciting_threshold` and tail-room ACK piggybacking without hurting live-media tail-loss recovery (rejected: piggybacking only with 25 ms default; drop).
- ✅ Default `max_ack_delay = 50 ms` (configurable), reserve tail room to piggyback ACKs at `max_ack_delay / 2`, pair with PTO `STREAM` duplication so +25 ms `max_ack_delay` doesn't cost an extra full RTT on tail loss, and expose `ack_eliciting_threshold` keeping its default at `1` (rejected: keeping 25 ms default; raising default `ack_eliciting_threshold` to 10/cwnd-scaled now).
- Joins `m1` as additive config fields on `#[non_exhaustive] pub struct transport::Config` + internal packet-packing and PTO improvements.

Guidance:

- **Configurable `max_ack_delay` past 25 ms**: Add `max_ack_delay` to `transport::Config` (default `50 ms`, advertised in the local `max_ack_delay` transport parameter instead of hardcoded `25 ms` at `transport_parameters.rs:62`), and remove the `Ord::min(candidate, Ord::max(rtt, 25ms))` clamp when an explicit `ack_frequency::Config::max_ack_delay` is requested (`ack_frequency.rs:55, 175`) so callers can request >25 ms (e.g. 50–120 ms) on low-RTT paths. Also expose `ack_eliciting_threshold` (default `1`) cleanly for workloads that want fewer ACKs on busy streams.
- **Tail-room ACK piggybacking (`connection/mod.rs:4024-4067`, `spaces.rs:656-714`)**: Today `streams.write_frames` consumes up to `max_size` before `try_populate_acks`, leaving 0 bytes on full-MTU packets. When `pending_acks` has unacked packets and at least `max_ack_delay / 2` has elapsed since `earliest_ack_eliciting_since_last_ack_sent` (or whenever space naturally remains), reserve room (or write the ACK before filling the rest with `STREAM` frames) so the ACK rides on outgoing data instead of spawning a standalone ACK-only packet. Keep immediate dedicated ACKs on loss/reordering, ECN CE, `IMMEDIATE_ACK`, and `max_ack_delay` expiry.
- **Exact `SendBuffer::fit` length varint (`send_buffer.rs:36-50`)**: Subtract the exact `VarInt::size` (1–2 bytes for <= 16 KiB packets) rather than `8`, eliminating avoidable 1–6 byte trailing STREAM splits.
- **Accurate ACK delay (`connection/mod.rs:2594`)**: Pass `self.arrival.unwrap_or(now)` into `PendingAcks::insert_one` so event-loop scheduling lag isn't folded into peer RTT.
- **PTO `STREAM` duplication (`spaces.rs:140-156`)**: In `maybe_queue_probe`, when no new data or control retransmits are queued, duplicate the oldest in-flight `STREAM` frame (reusing the `streams.write_duplicates` machinery from `bandwidth_probe`) before falling back to `PING`/`IMMEDIATE_ACK`, cutting tail-loss recovery by a full RTT.
- **Verification**: Add sim tests measuring packet and ACK counts on bidirectional and unidirectional media streams (confirming fewer standalone ACKs and faster tail-loss recovery on bufferbloat links) and verify compatibility with existing interop tests.

Public API: additive (new field(s) on `#[non_exhaustive] transport::Config`). Wire: none (standard RFC 9000 `max_ack_delay` transport parameter and `draft-ietf-quic-ack-frequency`).

## Required

- [ACK, ECN, `ACK_FREQUENCY`, and `ReceiveTimestamps` fixes](/quest/m1/quic-ack-and-ecn-fixes.md) - fixes lost `ACK_FREQUENCY` retransmits and `ReceiveTimestamps` exact-fit drops before changing ACK policy

## Related

- [Per-stream deadlines and proactive TLP](/quest/m4/quic-deadline.md) - proactive TLP ahead of PTO for deadline-bearing streams
- [Packet, ACK, and syscall count tracking](/quest/m4/quic-packet-count-metrics.md) - guards packet and ACK ratios across sim and `moq-bench`
