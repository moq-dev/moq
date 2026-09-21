# [L] Probe capacity by early retransmission

## Goal

The selected QUIC backend discovers egress headroom by retransmitting recent
in-flight data early instead of sending padding. Probe bytes are useful if the
original was lost and cost no more than padding if it was not. The resulting
capacity estimate flows through the existing transport estimate, MoQ PROBE,
and publisher rate adaptation.

## Plan

Implement the opt-in mechanism in the fork's recovery and pacing layer.
Mark probe packets for an `IMMEDIATE_ACK` (the ACK-frequency extension noq
already implements) so a capacity sample is not delayed by the peer's ACK
timer. Enable it only while the application consumes bandwidth estimates, so
idle connections never probe. Exclude streams or packets that have already
expired under MoQ's group lifetime.

Keep accounting two-sided and truthful. A probe acknowledged beside its
original is not loss, and its bytes are not new application delivery. Its ACK
must feed a wire-capacity sample distinct from goodput, because an
application-limited sender cannot otherwise estimate capacity above its
encoder rate.

Measure cadence, step size, and interaction with the selected congestion
controller. Compare against padding, no probing, and ordinary loss-triggered
retransmission under clean, random-loss, and short-burst-loss profiles. Require
stable application latency and no double-counted delivery before exposing the
option through the backend-neutral estimate.

## Required

- [Fork noq](/quest/next/quic/fork.md) - the recovery and pacing change lives
  there

## Related

- [FEC experiment](/quest/future/quic-fec.md) - early retransmission is a
  repetition code competing for the same redundancy budget
- [GCC egress experiment](/quest/future/quic-gcc.md) - a delay-based controller
  changes what headroom means
- [BBR3 app-limited](/quest/future/quic-bbr-app-limited.md) - whether the
  estimate an app-limited sender already has is trustworthy
