# [L] QUIC GCC egress experiment

## Goal

Record a measured verdict on WebRTC-style delay-based congestion control for
subscriber-facing media egress against noq's production
controller. Ship it only if it reduces queueing delay and rate variation
without collapsing throughput. A written abandonment is a successful outcome.

## Plan

Implement the candidate in the fork as a `congestion::Controller`, driven by
the per-packet receive timestamps the receive-timestamps spike delivers; the
sender-side inter-arrival filter is what makes it GCC rather than another
RTT-based controller. If it ships, it joins MoQ's backend-neutral congestion
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

## Required

- [Receive timestamps](/quest/m2/quic-receive-ts.md) - the per-packet
  arrival times the delay filter runs on
