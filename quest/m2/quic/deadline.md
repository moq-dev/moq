# [L] Per-stream deadlines

## Goal

A send stream can carry a deadline. Bytes that cannot reach the peer before it
are not retransmitted: the stream is reset instead, so a late group never
competes with a live one for the congestion window. Below the deadline,
recovery gets faster rather than slower: when the last packet of a burst is
still unacknowledged and there is time for an acknowledgment and one
retransmission to land, the sender asks for an immediate ACK and probes early
instead of waiting a full PTO. moq-net sets the deadline per group stream from
the subscription's latency target and the group's expiry, so no MoQ wire
change is needed.

## Plan

Implement in the fork.

- Add `set_deadline(Instant)` on `SendStream`. A stream without one behaves
  exactly as today.
- On loss detection, before queueing a retransmission for a stream with a
  deadline, estimate the arrival instant as now plus the forward one-way
  delay. Start with `min_rtt / 2`, corrected by the peer's reported ACK delay;
  the [receive-timestamps spike](/quest/m3/quic-receive-ts.md) replaces that
  guess with a measured forward delay. If the estimate is past the deadline,
  reset the stream with a dedicated error code and drop its retransmit ranges,
  including bytes already lost, so flow control is returned in one step.
- Proactive tail-loss probe: when the newest in-flight packet carries deadline
  data and `now + pto() > deadline - rtt`, send an `IMMEDIATE_ACK` (the
  ACK-frequency extension noq already implements) on the next packet and arm
  a shortened probe at `max(deadline - rtt - now, min_pto)`. Never probe past
  the congestion window; the probe is a scheduling choice, not extra credit.
- moq-net: `Subscription::serve_group` sets the deadline from the
  subscription's latency target and the group's expiry, whichever is sooner;
  a subscription with neither sets none. The reset error code maps to the
  existing group-expired code on the MoQ wire, so a viewer sees the same
  signal it sees for a relay-side expiry today.

Measure on the impaired path profile: delivery latency p95 and p99, bytes
retransmitted past their deadline (must be zero), spurious resets under
reordering, and probe overhead versus the default PTO. A proactive probe that
raises loss or latency under any profile stays off by default.

## Required

- [Fork noq](/quest/m2/quic/fork.md) - the change lives there
- [Hierarchical stream scheduling](/quest/m2/quic/scheduler.md) - the
  scheduler decides which stream's data a probe carries

## Related

- [Receive timestamps](/quest/m3/quic-receive-ts.md) - a measured forward
  delay replaces the half-RTT estimate
- [Probe by early retransmission](/quest/m2/quic/probe.md) - shares the
  retransmit-as-probe machinery
