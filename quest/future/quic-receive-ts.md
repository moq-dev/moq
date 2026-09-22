# [M] Receive timestamps

## Goal

A measured verdict on the QUIC receive-timestamps extension
(draft-smith-quic-receive-ts): the peer reports when each acknowledged packet
arrived, so the sender sees per-packet one-way delay variation the way
WebRTC's transport-wide congestion control feedback does, instead of inferring
it from ACK arrival on the return path. The result is either a released fork
feature with the `Controller` trait carrying per-packet receive instants, or a
written reason it does not pay.

## Plan

- Implement the extension in the fork: the transport parameter, the ACK
  frame variant with timestamp ranges, and an `on_ack` extension on
  `Controller` that hands each acknowledged packet its receive instant. Both
  ends are ours; browsers never see it.
- Compare the forward delay it measures against the half-RTT estimate the
  [deadline quest](/quest/next/quic/deadline.md) starts with, on the impaired
  path profile with asymmetric delay. Report how often the half-RTT estimate
  would have kept a hopeless retransmission or reset a deliverable one.
- Measure the ACK overhead the timestamps add on a fanout-shaped relay egress,
  with and without the ACK-frequency extension reducing ACK rate.

The GCC experiment requires this; a delay-based controller without per-packet
arrival times is a different, weaker experiment.

## Related

- [QUIC GCC](/quest/future/quic-gcc.md) - the controller that consumes it
- [Per-stream deadlines](/quest/next/quic/deadline.md) - the other consumer
