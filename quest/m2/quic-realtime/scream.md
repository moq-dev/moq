# [L] SCReAMv2 candidate

## Goal

The SCReAMv2 (RFC 8298bis) delay-based path runs as a
controller in `moq-congestion`, fed by receive timestamps, and reports
its queuing delay, throughput, and reaction time on the shared moq-shaper
uplink presets.

## Plan

No Rust implementation exists; port from the specification, using Ericsson's
BSD-licensed reference (EricssonResearch/scream) to check behavior.

- Port only the queue-delay target path. The L4S/ECN path waits on
  [the ECN measurement](/quest/m2/quic-ecn-measure.md): an ECN path between
  publishers and relays is unproven, and the shaper cannot mark.
- SCReAM's queue delay comes from one-way delay samples; use the receive-ts
  forward delay with its basis offset removed (the spike's estimator), not
  absolute time.
- Implement `Controller::probes` and report `probing` so moq-quic's
  native probing still raises the estimate (decided 2026-10-25, maintainer;
  see the [epic](/quest/m2/quic-realtime/README.md)). Measure that a stopped
  rung recovers under it.
- Unit tests drive the controller with mocked time, not wall clock.

## Required

- [Receive-ts spike](/quest/m2/quic-realtime/receive-ts.md) - the per-packet receive instants and the shared presets
