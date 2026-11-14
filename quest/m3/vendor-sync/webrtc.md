# [S] The WebRTC ports have every upstream fix they need

## Goal

moq-congestion's `gcc` module (GoogCC) and moq-audio's NetEq time stretch
(`rs/moq-audio/src/stretch.rs`) carry every fix WebRTC has landed in their
source files since the last check, each ported with its test or recorded as
not applying, and each module doc names the newest WebRTC revision checked.

## Plan

Recurring child of the [vendor sync](/quest/m3/vendor-sync/README.md), which
says how each run works and recurs. New in the 2026-11-05 scan: both ports
landed after the first run started.

GoogCC: `rs/moq-congestion/src/gcc/mod.rs` records WebRTC
[`556f7eec27`](https://webrtc.googlesource.com/src/+/556f7eec27) (2026-11-01),
refreshed in [moq-dev/moq#7546](https://github.com/moq-dev/moq/pull/7546) from
[goog_cc](https://github.com/kixelated/goog_cc) 0.1.4. Each file follows its
upstream counterpart, so diff WebRTC's `modules/congestion_controller/goog_cc`,
plus the `api/transport`, `api/units`, `modules/pacing`, and
`modules/remote_bitrate_estimator` files it ports, from that revision and
apply the changes file by file. Keep the recorded departures (the ALR
detector's per-send rounding, the 1 Mbit/s start, restarted probe clusters)
unless upstream fixes what they work around. GoogCC is a candidate in the
[realtime uplink bake-off](/quest/m3/quic-realtime/README.md); if the bake-off
deletes it, drop that half.

NetEq: [moq-dev/moq#7856](https://github.com/moq-dev/moq/pull/7856) ported
`time_stretch`, `accelerate`, `preemptive_expand`, `dsp_helper`,
`audio_vector`, and `decision_logic` under
`modules/audio_coding/neteq/`, reading `main` on 2026-11-04 without recording
a revision. Start from the last WebRTC `main` commit on 2026-11-04, and record
the revision checked in `stretch.rs`'s module doc. The port is a deliberate
floating-point adaptation (maintainer, 2026-11-04), so an upstream change to
Q15 arithmetic alone does not apply; `doc/concept/audio-jitter.md` and its PCM
corpus define the algorithm, so update both with any ported change. The
[lag rule](/quest/m2/audio-playout/lag-rule.md) replaces the port with a
continuous time stretch before the cut; once it lands, drop that half.

WebRTC's gitiles history pages need a Google sign-in; clone
`https://webrtc.googlesource.com/src` instead.

## Required

- [Cut and publish the prepared breaking release](/quest/m3/release/cut.md) - the next cycle starts after the cut
