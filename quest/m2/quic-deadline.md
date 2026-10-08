# [L] Per-stream deadlines

## Goal

A send stream can carry a deadline. Bytes that cannot reach the peer before it
are not retransmitted: the stream is reset instead, so a late group never
competes with a live one for the congestion window. Below the deadline,
recovery gets faster rather than slower: when the last packet of a burst is
still unacknowledged and there is time for an acknowledgment and one
retransmission to land, the sender asks for an immediate ACK and probes early
instead of waiting a full PTO. moq-net sets the deadline per group stream from
the subscription's `max_delay` and the group's expiry, so no MoQ wire
change is needed.

## Plan

Implement in the fork.

- Add `set_deadline(Instant)` on `SendStream`. A stream without one behaves
  exactly as today.
- On loss detection, before queueing a retransmission for a stream with a
  deadline, estimate the arrival instant as now plus the forward one-way
  delay. Start with `min_rtt / 2`, corrected by the peer's reported ACK delay;
  the [GCC experiment](/quest/m3/quic-gcc.md)'s receive timestamps replace that
  guess with a measured forward delay. If the estimate is past the deadline,
  reset the stream with a dedicated error code and drop its retransmit ranges,
  including bytes already lost, so flow control is returned in one step.
- Proactive tail-loss probe: when the newest in-flight packet carries deadline
  data and `now + pto() > deadline - rtt`, send an `IMMEDIATE_ACK` (the
  ACK-frequency extension `moq-quic` already implements) on the next packet and arm
  a shortened probe at `max(deadline - rtt - now, min_pto)`. Never probe past
  the congestion window; the probe is a scheduling choice, not extra credit.
- moq-net cannot call `moq-quic` directly, so the deadline crosses its own
  `transport::poll::SendStream` trait, the way
  [the ACK hook](/quest/m2/quic-ack-hook.md) does: a method whose default
  reports unsupported, implemented by the moq-tokio and moq-uring adapters
  over `moq-quic`. A backend without it sends as today.
- moq-net: the per-group `GroupServe` machine in the lite and IETF
  publishers (`lite/publisher.rs`, `ietf/publisher.rs`) sets the deadline
  when it opens the stream, from the subscription's `max_delay` and the
  group's expiry, whichever is sooner; a subscription with neither sets none. The reset error code maps to the
  existing group-expired code on the MoQ wire, so a viewer sees the same
  signal it sees for a relay-side expiry today.

Measure on the impaired path profile: delivery latency p95 and p99, bytes
retransmitted past their deadline (must be zero), spurious resets under
reordering, and probe overhead versus the default PTO. A proactive probe that
raises loss or latency under any profile stays off by default.

Decided in the 2026-09-30 audit: moved to m2. No m1 quest consumes it.

## Required

- [Hard fork](/quest/m1/quic/fork/README.md) - the change lands in `moq-quic`, not the frozen fork
- [Hierarchical stream scheduling](/quest/m1/quic/scheduler.md) - the
  scheduler decides which stream's data a probe carries

## Related

- [QUIC GCC](/quest/m3/quic-gcc.md) - its receive timestamps give a measured
  forward delay that replaces the half-RTT estimate
- [ACK hook](/quest/m2/quic-ack-hook.md) - the same pattern for a moq-net method backed by `moq-quic`
- [Discover media headroom](/quest/m2/quic-probe.md) - can reuse
  retransmission machinery if redundant capacity probes prove worthwhile
- [noq#813](https://github.com/n0-computer/noq/issues/813) - the per-stream deadline proposal to n0
