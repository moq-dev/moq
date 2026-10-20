# [L] GCC candidate

## Goal

libwebrtc's Google Congestion Control runs as a `congestion::Controller` in
`moq-quic`, fed by receive timestamps, and reports its queuing delay,
throughput, and reaction time on the shared moq-shaper uplink presets.

## Plan

Decided 2026-10-17: vendor [goog_cc](https://github.com/kixelated/goog_cc)
into `rs/moq-quic/src/congestion/gcc/` and refresh it to current WebRTC rather
than depend on the crates.io 0.1.4 release. The port predates AI-assisted
review and tracks a 2024-12-09 WebRTC snapshot, so owning it in-tree lets us
trim it to what QUIC needs and review it the way we review the rest of the
fork.

- Refresh against current `modules/congestion_controller/goog_cc` and record
  the WebRTC revision in the module docs.
- Trim what QUIC makes redundant: RTP packet feedback types become the
  Controller's per-packet send and receive instants; drop the parts that only
  serve RTCP or probe clusters unless the comparison needs them.
- GCC outputs a target rate and pacing rate; map them to the Controller's
  window and `ControllerMetrics::{pacing_rate, bandwidth_estimate}`, keeping
  its congestion-window pushback as the inflight bound.
- Keep the BSD license and PATENTS file with the vendored code.
- Unit tests drive the controller with mocked time, not wall clock.

## Required

- [Receive-ts spike](/quest/m2/quic-realtime/receive-ts.md) - the per-packet receive instants and the shared presets
