# [XL] QUIC GCC egress experiment

## Goal

Record a measured verdict on WebRTC-style delay-based congestion control for
subscriber-facing media egress against noq's production
controller, driven by the QUIC receive-timestamps extension
(draft-smith-quic-receive-ts): the peer reports when each acknowledged packet
arrived, so the sender sees per-packet one-way delay variation the way
WebRTC's transport-wide congestion control feedback does. Ship it only if it reduces queueing delay and rate variation
without collapsing throughput. A written abandonment is a successful outcome.

## Plan

### Receive timestamps

- Implement the extension in the fork: the transport parameter, the ACK
  frame variant with timestamp ranges, and an `on_ack` extension on
  `Controller` that hands each acknowledged packet its receive instant. Both
  ends are ours; browsers never see it.
- Compare the forward delay it measures against the half-RTT estimate the
  [deadline quest](/quest/m2/quic-deadline.md) starts with, on the impaired
  path profile with asymmetric delay. Report how often the half-RTT estimate
  would have kept a hopeless retransmission or reset a deliverable one.
- Measure the ACK overhead the timestamps add on a fanout-shaped relay egress,
  with and without the ACK-frequency extension reducing ACK rate.

A delay-based controller without per-packet arrival times is a different,
weaker experiment.

### GCC

Implement the candidate in the fork as a `congestion::Controller`, driven by
those per-packet receive timestamps; the sender-side inter-arrival filter is
what makes it GCC rather than another RTT-based controller. If it ships, it joins MoQ's backend-neutral congestion
family as `CongestionControl::RealTime`, beside `Loss` (Cubic) and `Delay`
(BBR3); MoQ owns which algorithm each name means, and the config never
exposes algorithm names. Egress only: relay ingest keeps BBR3.

Use the moq-bench media profiles under reproducible netem delay, loss, and
bottleneck rates. Decide on p95 queueing delay, delivered-rate variation,
goodput, and starvation against a competing low-rate interactive MoQ stream.
Anything measured on real relay hardware needs a dedicated real-NIC rig rather
than loopback.

State the experiment's boundary beside the result: netem cannot establish
behavior against production cross traffic or real wifi and cellular loss.

Receive timestamps are native-only: browsers never negotiate the extension,
so GCC can only target native peers or relay-to-relay sessions, not browser
egress. Decided 2026-09-30: the receive-timestamps spike folds in here, and
neither waits on a native consumer.

## Required

- [Hard fork](/quest/m1/quic/fork/README.md) - the change lands in `moq-quic`, not the frozen fork

## Related

- [noq#818](https://github.com/n0-computer/noq/issues/818) - the GCC proposal to n0; matheus23 asked for a non-breaking `Controller` trait
