# [M] QUIC receive-timestamps spike

## Goal

A measured, written verdict on the QUIC receive-timestamps extension
(draft-smith-quic-receive-ts) between native `moq-quic` peers: how much more
accurate its per-packet one-way delay is than the half-RTT estimate the
[deadline quest](/quest/m2/quic-deadline.md) starts with, and what its ACK
overhead costs on relay egress. A written abandonment is a successful
outcome.

## Plan

Decided 2026-10-08: shrunk from a full GCC egress experiment to this spike.
GCC had no consumer, and receive timestamps are native-only (browsers never
negotiate the extension), so it could never reach browser egress, which was
the experiment's point. The deadline quest is the one consumer that wants the
measurement. A delay-based controller built on the timestamps is re-planned
as its own quest only if this spike says they are cheap and accurate.

- Implement the extension in `moq-quic`: the transport parameter, the ACK
  frame variant with timestamp ranges, and a way for `congestion::Controller`
  (or a test hook) to see each acknowledged packet's receive instant. Both
  ends are ours.
- Compare the forward delay it measures against the half-RTT estimate on the
  impaired path profile with asymmetric delay. Report how often the half-RTT
  estimate would have kept a hopeless retransmission or reset a deliverable
  one.
- Measure the ACK overhead the timestamps add on a fanout-shaped relay egress,
  with and without the ACK-frequency extension reducing ACK rate.

Use the moq-bench media profiles under reproducible netem delay, loss, and
bottleneck rates. State the boundary beside the result: netem cannot
establish behavior against production cross traffic or real wifi and
cellular loss.

## Required

- [Hard fork](/quest/m1/quic/fork/README.md) - the extension lands in `moq-quic`

## Related

- [Per-stream deadlines](/quest/m2/quic-deadline.md) - the forward-delay estimate this measures against
