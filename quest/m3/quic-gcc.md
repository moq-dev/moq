# [M] QUIC receive-timestamps spike

## Goal

A measured, written verdict on the QUIC receive-timestamps extension
(draft-ietf-quic-receive-ts) between native `moq-quic` peers: whether its
receive-time differences improve the forward-delay estimate used by the
[deadline quest](/quest/m2/quic-deadline.md), and what its ACK
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
- Receive timestamps alone measure delay variation, not absolute one-way
  delay: the receiver's clock origin is not communicated (the draft's
  section 6.1). State clock offset and drift assumptions. Compare an explicit
  estimator against known simulator/netem delay and the half-RTT baseline on
  asymmetric paths; do not label raw receive timestamps absolute delay.
  Report mistaken retransmit/reset decisions and estimator uncertainty.
- Measure the ACK overhead the timestamps add on a fanout-shaped relay egress,
  with and without the ACK-frequency extension reducing ACK rate.

Use the moq-bench media profiles under reproducible netem delay, loss, and
bottleneck rates. State the boundary beside the result: netem cannot
establish behavior against production cross traffic or real wifi and
cellular loss.

## Related

- [Per-stream deadlines](/quest/m2/quic-deadline.md) - the forward-delay estimate this measures against
