# [M] Server and push sinks handle a broadcast's restarts

## Goal

Every moq-cli sink that resolves a broadcast and keeps serving it handles an
epochless `Restart` on purpose: it follows the new publisher instance where
its output can mark the break, and otherwise fails loud rather than ending as
if the broadcast had finished.

## Plan

The stdout exports already do this: TS follows with `--stitch`, and the other
formats exit 1 once the replaced instance ends (`Replacement` in
`rs/moq-cli/src/subscribe.rs`). Audit and fix the rest the same way:

- `export hls`: `moq-hls`'s server resolves per request; check that a restart
  moves new requests to the new instance and how a playlist marks the break
  (`EXT-X-DISCONTINUITY`). Coordinate with
  [HLS export failure signal](/quest/m1/hls-failure-signal.md).
- `export rtmp`, `export srt`, `export rtc`, both `--listen` (per-viewer
  streams in `moq-rtmp`, `moq-srt`, `moq-rtc`) and `--connect` (one push).
  SRT already rides the TS follower (`--linger`, `--stitch`); confirm its
  listen side does too. FLV and WebRTC need a decision.
- `export archive`: a restart already fails a continued store under a new
  epoch; decide what a mid-recording restart does.
- `transcode`: its source broadcast; following means restarting the ladder
  on the new instance.

Share one helper (the stdout exports' `Replacement`, or a `moq_mux`
`Source::follow`-style API per
[catalog references](/quest/m0/broadcast-epoch/catalog-references.md))
rather than one rule per sink. Each fix lands with a test that replaces the
publisher while the sink runs.

Public API: possibly a shared follow helper in `moq-mux`. Wire: none.
