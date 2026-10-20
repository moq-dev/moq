# [M] Server and push sinks handle a broadcast's restarts

## Goal

Every moq-cli sink that resolves a broadcast and keeps serving it handles an
epochless `Restart` on purpose: it follows the new publisher instance where
its output can mark the break. A route change alone is never fatal: a sink
that cannot follow stays on the old instance while its publisher serves it,
and fails once that route dies, since its subscriptions then end with an
error.

## Plan

The stdout exports already do this: TS follows with `--stitch`, and the other
formats ride the old instance until its route dies and its tracks error. Audit
and fix the rest the same way:

- `export hls`: `moq-hls`'s server resolves per request; check that a restart
  moves new requests to the new instance and how a playlist marks the break
  (`EXT-X-DISCONTINUITY`). Coordinate with
  [Failed HLS exports error](/quest/m1/hls-failure-signal.md).
- `export rtmp`, `export srt`, `export rtc`, both `--listen` (per-viewer
  streams in `moq-rtmp`, `moq-srt`, `moq-rtc`) and `--connect` (one push).
  SRT already rides the TS follower (`--linger`, `--stitch`); confirm its
  listen side does too. FLV and WebRTC need a decision.
- `export archive`: a restart already fails a continued store under a new
  epoch; decide what a mid-recording restart does.
- `transcode`: its source broadcast; following means restarting the ladder
  on the new instance.

Share one helper (a `moq_mux` `Source::follow`-style API per
[catalog references](/quest/m0/broadcast-epoch/catalog-references.md))
rather than one rule per sink. Each fix lands with a test that replaces the
publisher while the sink runs.

Public API: possibly a shared follow helper in `moq-mux`. Wire: none.

## Required

- [moq-net follows announced instances](/quest/m1/net-announced-follow.md) - the shared follow helper builds on moq-net's announced request, not a third hand-rolled loop
