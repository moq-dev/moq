# Realtime uplink congestion control

## Goal

A native publisher (moq-cli, OBS, GStreamer, moq-ffi) sending to a relay over
`moq-quic` can pick `CongestionControl::Realtime`. When both ends negotiate
draft-ietf-quic-receive-ts, a delay-based controller keeps the uplink
bottleneck queue near empty, so glass-to-glass latency stays near propagation
delay where BBR3 builds a standing queue, accepting lower throughput against
competing loss-based flows. This is for teleoperation uplinks (drones, robots)
over cellular and radio links, where a standing queue is control latency.

Browsers and every other egress keep their controller.
Without the extension, `Realtime` falls back to BBR3.

## Plan

Decided with the maintainer on 2026-10-17 (the 2026-10-08 shrink to a bare
spike assumed no consumer; teleoperation uplinks are one):

- Native publisher ingest only. Chrome's quiche can parse the frame but never
  negotiates it, so browser egress gains nothing, and browser publishers run
  Chrome's controller. Relay-to-relay links are well provisioned and stay on
  BBR3. The relay only *sends* timestamps, to peers that advertise
  `max_receive_timestamps_per_ack`; it never requests them, so relay egress
  ACKs are unchanged.
- Win on queuing delay, not fairness: a queue near empty at the bottleneck,
  accepting a smaller share against loss-based cross traffic.
- [The receive-ts spike](/quest/m2/quic-realtime/receive-ts.md) gates the
  candidates. A negative verdict on the forward-delay signal switches the
  candidates to RTT gradients rather than abandoning the epic.
- Bake-off: all three candidates in parallel, each a controller
  in `moq-congestion`. The spike's moq-shaper uplink
  presets decide it; the report states the shaper's timing jitter as the
  noise floor of the delay signal.
- Native probing is required (decided 2026-10-25, maintainer). Every
  candidate implements `moq_congestion::Controller::probes` and reports
  `probe_inflight`, so moq-quic's native probing
  ([moq-dev/moq#6635](https://github.com/moq-dev/moq/pull/6635)) still
  raises its estimate, and the bake-off measures probing alongside queuing
  delay. Reason: the [native rung disable](/quest/m1/simulcast-rung-disable.md)
  only stops a rung on a session with a sender, so a `Realtime` that cannot
  probe would silently leave moq-cli, OBS, and FFI simulcast publishers with
  no rung disable or recovery. A candidate that cannot probe does not ship.
- Naming: add `Realtime` to `moq_tokio::quic::CongestionControl` (`realtime`
  on the CLI and in TOML), which is `non_exhaustive`, so it is additive.
  `Delay` keeps meaning BBR3; reword its doc line to say model-based, since
  BBR does not react to queuing delay directly. A new variant rather than
  upgrading `Delay`, so current users see no behavior change.
- Docs: inline only. The congestion-control lines in `doc/bin/cli.md` and
  `doc/lib/*` gain the new variant; no new page.

Remaining work once the candidates finish (this README's own job):

- Run the shared comparison on the uplink presets: queuing delay p50/p95/p99,
  throughput, reaction time to a capacity step down, and share against one
  competing CUBIC flow. Pick the winner on queuing delay.
- Ship the winner as `Realtime` in moq-tokio and moq-uring, mapped through
  `MoqCongestionControl` in moq-ffi and every binding in the cross-package
  sync table, and a `congestion-control` property on `moqsink` forwarded
  into its QUIC config (moq-gst has no CLI or TOML loader).
- Delete the losing candidates' code.
- Wire the winner's moq-shaper run into the nightly so a regression in
  queuing delay shows up.

The winner's target rate already reaches moq-cli and moq-ffi encoders
through `bandwidth_estimate` → `estimated_send_rate` → `set_bitrate`.
`moqsink` only exposes a read-only `estimated-send-rate` and never retunes
the upstream encoder, so a GStreamer pipeline at a fixed bitrate still queues
when capacity drops; [its own quest](/quest/m2/quic-realtime/moqsink-feedback.md)
owns that feedback.

## Required

- [Receive-ts spike](/quest/m2/quic-realtime/receive-ts.md) - the extension in `moq-quic` and a verdict on its forward-delay signal
- [GCC candidate](/quest/m2/quic-realtime/gcc.md) - libwebrtc's GoogCC vendored into `moq-congestion` and refreshed to current WebRTC
- [BBR3 delay bound](/quest/m2/quic-realtime/bbr-delay-bound.md) - BBR3 with inflight capped when the forward delay gradient shows queue growth
- [SCReAMv2 candidate](/quest/m2/quic-realtime/scream.md) - the RFC 8298bis delay path ported to `moq-congestion`
- [moqsink encoder feedback](/quest/m2/quic-realtime/moqsink-feedback.md) - GStreamer publishers retune their encoder to the target rate

## Related

- [Per-stream deadlines](/quest/m2/quic-deadline.md) - also consumes the forward-delay estimate
- [BBR media study](/quest/m2/quic-bbr-study.md) - BBR3 behavior under media, the baseline here
- [Measure ECN on the backbone](/quest/m2/quic-ecn-measure.md) - decides whether SCReAMv2's L4S path is worth porting
- [QUIC on the sim](/quest/m2/quic-sim.md) - deterministic successor to the moq-shaper harness once it lands
