# Realtime uplink congestion control

## Goal

A native publisher (moq-cli, OBS, GStreamer, moq-ffi) sending to a relay over
`moq-quic` can pick `CongestionControl::Realtime`. When both ends negotiate
draft-ietf-quic-receive-ts, a delay-based controller keeps the uplink
bottleneck queue near empty, so glass-to-glass latency stays near propagation
delay where BBR3 builds a standing queue, accepting lower throughput against
competing loss-based flows. This is for teleoperation uplinks (drones, robots)
over cellular and radio links, where a standing queue is control latency.

Browsers, iroh/noq peers, and every other egress keep their controller.
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
- The receive-ts spike ([moq-dev/moq#7334](https://github.com/moq-dev/moq/pull/7334))
  landed draft-ietf-quic-receive-ts-03 in `moq-quic`: a session that sets
  `transport::Config::receive_timestamps` hands each controller the peer's
  receive time as `moq_congestion::Acked::received`. Its verdict was positive.
  A trendline over forward delay caught a capacity drop sooner than one over
  RTT, with fewer false alarms, whenever the ACK path added variance. Over
  `cellular-bursty` it was noisier than RTT, because per-packet delay sees the
  scheduler's grants. So the candidates group arrivals into bursts before
  fitting a trend, as GoogCC does. Reproduce it with `just rs quic-sim`.
- Bake-off: all three candidates in parallel, each a controller
  in `moq-congestion`. The moq-shaper presets `cellular-step`,
  `cellular-bursty` and `cellular-deep` decide it. The report states the
  shaper's timing jitter, measured over `near-zero`, as the noise floor of the
  delay signal.
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
- Run every candidate under both sender models in one harness: a bulk
  upload, and an encoder that follows `bandwidth_estimate` (the model moq-cli
  and moq-ffi use, and the one GoogCC is built for). Decided 2026-11-01
  (maintainer, follow-up planning after the 2026-11-01 /quest-afk run): the
  candidates' own runs each used one model, so their rows don't compare.
  [moq-dev/moq#7544](https://github.com/moq-dev/moq/pull/7544)'s
  `tests::realtime` uploaded in bulk, which
  [moq-dev/moq#7546](https://github.com/moq-dev/moq/pull/7546) found
  understates GoogCC (low utilization from its 350 ms window allowance and
  additive increase), while
  [moq-dev/moq#7547](https://github.com/moq-dev/moq/pull/7547)'s
  `tests::scream` drove an adaptive encoder (30 fps, 2 s GOP, 5x keyframes).
- Before trusting a candidate's estimate to re-enable a stopped rung, weigh
  how far it overshoots the path under app-limited bursty media. SCReAMv2's
  estimate is its reference window over the RTT, not a delivery rate, so each
  keyframe burst grew it past the path: 241 kB/s on a 125 kB/s path in #7547,
  which let a rung with keyframes recover without a probe. The
  [native rung disable](/quest/m1/simulcast-rung-disable.md) could re-enable
  a rung the path can't carry on such an estimate, so the comparison reports
  each candidate's estimate against the path's capacity under the encoder
  model.
- Ship the winner as `Realtime` in moq-tokio and moq-uring, mapped through
  `MoqCongestionControl` in moq-ffi and every binding in the cross-package
  sync table, and a `congestion-control` property on `moqsink` forwarded
  into its QUIC config (moq-gst has no CLI or TOML loader). iroh maps
  `Realtime` to BBR3 (noq has no extension).
- Delete the losing candidates' code.
- Decide who asks for receive timestamps. The spike added
  `transport::Config::receive_timestamps`, off by default, and the maintainer
  kept it on 2026-10-30. The winner decides whether its `ControllerFactory`
  turns them on instead, so only `Realtime` sessions pay the ACK bytes.
- Wire the winner's moq-shaper run into the nightly so a regression in
  queuing delay shows up.

The winner's target rate already reaches moq-cli and moq-ffi encoders
through `bandwidth_estimate` → `estimated_send_rate` → `set_bitrate`.
`moqsink` only exposes a read-only `estimated-send-rate` and never retunes
the upstream encoder, so a GStreamer pipeline at a fixed bitrate still queues
when capacity drops; [its own quest](/quest/m2/quic-realtime/moqsink-feedback.md)
owns that feedback.

## Required

- [GCC candidate](/quest/m2/quic-realtime/gcc.md) - native probing raises GCC's estimate to the probe target, so a stopped rung recovers under it
- [BBR3 delay bound](/quest/m2/quic-realtime/bbr-delay-bound.md) - BBR3 with inflight capped when the forward delay gradient shows queue growth
- [SCReAMv2 candidate](/quest/m2/quic-realtime/scream.md) - the RFC 8298bis delay path ported to `moq-congestion`
- [io_uring runs an application's controller](/quest/m2/quic-realtime/uring-controller.md) - moq-uring takes a controller factory, so the winner installs the same way on both runtimes
- [moqsink encoder feedback](/quest/m2/quic-realtime/moqsink-feedback.md) - GStreamer publishers retune their encoder to the target rate

## Related

- [The shared pacer holds its rate under 2 Mbit/s](/quest/m1/pacer-low-rate.md) - every candidate paces through it, so its fix shifts their low-rate rows
- [Per-stream and per-datagram deadlines](/quest/m2/quic-deadline.md) - also consumes the forward-delay estimate
- [BBR media study](/quest/m2/quic-bbr-study.md) - BBR3 behavior under media, the baseline here
- [Measure ECN on the backbone](/quest/m2/quic-ecn-measure.md) - decides whether SCReAMv2's L4S path is worth porting
- [QUIC on the sim](/quest/m2/quic-sim.md) - deterministic successor to the moq-shaper harness once it lands
