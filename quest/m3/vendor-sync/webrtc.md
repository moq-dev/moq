# [S] The WebRTC GoogCC port has every upstream fix it needs

## Goal

moq-congestion's `gcc` module (GoogCC) carries every fix WebRTC has landed in
its source files since the last check, each ported with its test or recorded
as not applying, and its module doc names the newest WebRTC revision checked.

## Plan

Recurring child of the [vendor sync](/quest/m3/vendor-sync/README.md), which
says how each run works and recurs. New in the 2026-11-05 scan: the port
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

The NetEq time stretch port this quest also covered was replaced by a
continuous time stretch written from its published description, so nothing of
WebRTC's audio coding remains to sync.

WebRTC's gitiles history pages need a Google sign-in; clone
`https://webrtc.googlesource.com/src` instead.

## Required

- [Cut and publish the prepared breaking release](/quest/m3/release/cut.md) - the next cycle starts after the cut
