# [L] Per-stream and per-datagram deadlines

## Goal

A send stream can carry a deadline. Bytes that cannot reach the peer before it
are not retransmitted: the stream is reset instead, so a late group never
competes with a live one for the congestion window. Below the deadline,
recovery gets faster rather than slower: when the last packet of a burst is
still unacknowledged and there is time for an acknowledgment and one
retransmission to land, the sender asks for an immediate ACK and probes early
instead of waiting a full PTO. moq-net sets the deadline per group stream from
the subscription's `max_delay` and the group's expiry, so no MoQ wire
change is needed. Datagrams get the same: a queued datagram past its
deadline is dropped from the send buffer instead of sent late, so a
congested viewer receives fresh datagrams rather than seconds-old ones.

## Plan

Implement in `moq-quic`.

- Add `set_deadline(Instant)` on `SendStream`. A stream without one behaves
  exactly as today.
- On loss detection, before queueing a retransmission for a stream with a
  deadline, estimate the arrival instant as now plus the forward one-way
  delay. Start with `min_rtt / 2`, corrected by the peer's reported ACK delay.
  Measure whether receive-time differences (`moq_congestion::Acked::received`,
  from the [realtime epic](/quest/m3/quic-realtime/README.md)) improve that
  estimate, accounting for clock offset, drift, and uncertainty. They do not
  alone measure absolute one-way delay. If the estimate is past the deadline,
  reset the stream with a dedicated error code and drop its retransmit ranges,
  including bytes already lost, so flow control is returned in one step.
- Proactive tail-loss probe: when the newest in-flight packet carries deadline
  data and `now + pto() > deadline - rtt`, send an `IMMEDIATE_ACK` (the
  ACK-frequency extension `moq-quic` already implements) on the next packet and arm
  a shortened probe at `max(deadline - rtt - now, min_pto)`. Never probe past
  the congestion window; the probe is a scheduling choice, not extra credit.
- moq-net cannot call `moq-quic` directly, so the deadline crosses its own
  `transport::poll::SendStream` trait, the way
  [the ACK hook](/quest/m3/quic-ack-hook.md) does: a method whose default
  reports unsupported, implemented by the moq-tokio and moq-uring adapters
  over `moq-quic`. A backend without it sends as today.
- moq-net: the per-group `GroupServe` machine in the lite and IETF
  publishers (`lite/publisher.rs`, `ietf/publisher.rs`) sets the deadline
  when it opens the stream, from the subscription's `max_delay` and the
  group's expiry, whichever is sooner; a subscription with neither sets none. The reset error code maps to the
  existing group-expired code on the MoQ wire, so a viewer sees the same
  signal it sees for a relay-side expiry today.

Datagrams. Decided 2026-11-01 (maintainer, follow-up planning after the
2026-11-01 /quest-afk run), from
[moq-dev/moq#7517](https://github.com/moq-dev/moq/pull/7517)'s datagram
delivery bench: behind a 2 Mbit/s bottleneck fed 4 Mbit/s, about 25% of
datagrams were lost and the survivors' p99 was about 8.2 s (p50 about 50 ms)
on lite-06, lite-07-wip, and moq-transport-22 alike. Congestion control kept
the shaper's queue from overflowing; the sender's 1 MiB QUIC datagram buffer
absorbed the overload and dropped only on overflow, and nothing expired a
datagram by age. Of #7517's options (a send deadline, a smaller send buffer,
or oldest-first within a subscription), the deadline was picked, because it
uses the delay budget the subscriber already stated instead of a buffer size
or ordering that suits only some rates.

- moq-quic: a datagram carries an optional deadline, and the send path
  drops a queued datagram whose deadline has passed (counted, and reported
  in the connection's stats) before building a packet with it. A datagram
  without one behaves as today. The same estimate of forward delay applies:
  drop one that can't arrive before its deadline.
- moq-net: the datagram writer sets the deadline from the subscription's
  max delay (`Delay::max`), through the same `transport::poll` seam as the stream
  deadline; a backend without it sends as today.
- Keep newest-first within a subscription; with stale datagrams expiring,
  it no longer starves the fresh ones.
- Re-run #7517's `datagram_delivery` bench: p99 behind the bottleneck stays
  within the subscription's max delay, and the loopback viewer is unchanged.
  Percentiles are over delivered datagrams, so also report offered,
  delivered, and expired counts separately, and check that datagrams
  delivered within the max delay don't fall below today's: the deadline may
  drop only what would have arrived too late anyway.

Measure on bufferbloat: a rate below the media bitrate behind a deep but
finite queue whose overflow demonstrably drops packets, with competing
loss-based traffic if needed to fill it. Random loss may prove recovery
correctness, but never grades latency or throughput. Report delivery latency
p95 and p99, bytes
retransmitted past their deadline (must be zero), spurious resets under
reordering, and probe overhead versus the default PTO. A proactive probe that
raises loss or latency under any profile stays off by default.

Restored 2026-11-12 in m4 (maintainer, `/quest-import` of
[moq-dev/moq#8559](https://github.com/moq-dev/moq/issues/8559)) after
[moq-dev/moq#8423](https://github.com/moq-dev/moq/pull/8423) pruned it from
m5 for want of a consumer. The consumer: MPEG-TS trunks under a fixed
`moq export ts --delay`, which compete with SRT on delivering whatever can
still arrive within the delay. The outage loss that issue measured belongs to
[Buffered exports pick their group order by measurement](/quest/m1/export-order.md)
and [A path that's back is used within an RTT](/quest/m3/quic-path-return.md),
not this quest: the early probe never exceeds the congestion window, which is
full of unacknowledged packets during an outage. This quest is the
"below the deadline, recovery gets faster" half and the datagram expiry.
Rejected: restoring `quic-careful-resume` too, whose gain is reasoned, not
measured.

## Related

- [ACK hook](/quest/m3/quic-ack-hook.md) - the same pattern for a moq-net method backed by `moq-quic`
