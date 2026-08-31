# [L] Quiche GCC egress experiment

## Goal

A measured verdict on WebRTC-style delay-based congestion control (GCC) for
subscriber-facing media egress, against the current default (quiche's BBRv2
via `bbr2_gcongestion`). Ship it only if it wins on queueing delay and rate
smoothness without collapsing throughput; a written-down abandonment is a
successful outcome. Findings are recorded in this Plan.

## Plan

- Implemented as a congestion-control algorithm in the moq-dev/quiche fork,
  where all fork work lands, selectable by name like the existing set, and
  slotted under moq's backend-neutral `loss`/`delay` knob
  (`moq-tokio/src/quic.rs` deliberately names families, not algorithms,
  because every backend ships a different generation). moq-uring carries its
  own separate `Congestion` enum today; whichever knob survives, GCC joins the
  `delay` family behind it.
- Egress only: the relay's send path to subscribers. Ingest keeps the
  default.
- Rig: moq-bench media profiles (the moq#2875 M0 suite) under netem-shaped
  delay, loss, and bottleneck rates; the metrics that decide are p95 queueing
  delay, delivered-rate stability, and starvation versus a competing flow.
  Anything measured on real relay hardware needs a dedicated real-NIC rig
  rather than loopback.
- What this cannot establish: behavior against real cross traffic at
  scale, and last-mile wifi/cellular realism; netem shapes are a model. State
  that limit next to every number.
