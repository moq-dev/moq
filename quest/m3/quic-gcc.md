# [L] QUIC GCC egress experiment

## Goal

Record a measured verdict on WebRTC-style delay-based congestion control for
subscriber-facing media egress against noq's production
controller. Ship it only if it reduces queueing delay and rate variation
without collapsing throughput. A written abandonment is a successful outcome.

## Plan

Implement the candidate in noq and expose it under
moq's backend-neutral `delay` congestion family. Egress only: relay ingest
keeps the production controller.

Use the moq-bench media profiles under reproducible netem delay, loss, and
bottleneck rates. Decide on p95 queueing delay, delivered-rate variation,
goodput, and starvation against a competing low-rate interactive MoQ stream.
Anything measured on real relay hardware needs a dedicated real-NIC rig rather
than loopback.

State the experiment's boundary beside the result: netem cannot establish
behavior against production cross traffic or real wifi and cellular loss.

## Required

- [noq parity gate](/quest/m2/quic/noq-parity.md) - experiment on the
  backend intended for production
