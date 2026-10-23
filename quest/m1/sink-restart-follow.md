# [L] moq-cli and moq-gst readers follow a broadcast's restarts

## Goal

Every moq-cli and moq-gst reader that resolves a broadcast and keeps reading
it follows publisher instances through moq-net's
`origin::Consumer::announced_broadcast`
([#5925](https://github.com/moq-dev/moq/pull/5925)) instead of a hand-rolled
`origin.follow` loop. Each handles an epochless
`Restart` on purpose: it follows the new instance where its output can mark
the break. A route change alone is never fatal: a reader that cannot follow
stays on the old instance while its publisher serves it, and fails once that
route dies, since its subscriptions then end with an error.

## Plan

Decided with the maintainer on 2026-10-20: widened from the server and push
sinks to every moq-cli and moq-gst reader. Following a catalog's references
to other broadcasts is [moqsrc sibling follow](/quest/m0/broadcast-epoch/moqsrc-sibling-follow.md)
for `moqsrc`, through `moq_mux::Source::follow`;
`moq play` resolves a reference once with `Source::resolve`, which waits for
a sibling's first announcement and keeps it.

Readers with their own follow loop today move onto `announced_broadcast`:

- moqsrc: `follow_path` in `rs/moq-gst/src/source/imp.rs` drains
  `origin.follow` and re-requests on each start, holding its pads across
  instances.
- `moq play`: `play` in `rs/moq-cli/src/play/media.rs` does the same and
  resets its presentation per instance.
- The TS stitching follower (`rs/moq-mux/src/container/ts/follower.rs`), if
  `announced_broadcast` fits how it marks a break.

A finite read such as `fetch` stays on the one instance it resolved.

The stdout exports (`subscribe`) already follow only where the output marks
the break: TS with `--stitch`. Every other format, and TS without `--stitch`,
stays pinned to the old instance until its route dies and its tracks error,
never concatenating independent instances into one stream. Keep that rule.
Audit and fix the rest the same way:

- `export hls`: `moq-hls`'s server resolves per request; check that a restart
  moves new requests to the new instance and how a playlist marks the break
  (`EXT-X-DISCONTINUITY`); a failed timeline answers its playlists with a no-store 5xx.
- `export rtmp`, `export srt`, `export rtc`, both `--listen` (per-viewer
  streams in `moq-rtmp`, `moq-srt`, `moq-rtc`) and `--connect` (one push).
  SRT already rides the TS follower (`--linger`, `--stitch`); confirm its
  listen side does too. FLV and WebRTC need a decision.
- `transcode`: its source broadcast; following means restarting the ladder
  on the new instance.

Share `moq_mux::follow::Instances::new(origin, path, None)` (decided 2026-10-20,
maintainer: no reader hand-rolls its own follow loop) rather than one rule per
reader: it yields `follow::Event::Start` for each instance serving the path and
`follow::Event::End` once the one handed out stops serving. Build a
`moq_mux::Source` from each started instance, so its siblings follow through
`Source::follow` and its exporters pin through `Source::bind`, which waits for
a sibling's first announcement. Each fix lands
with a test that replaces the publisher while the reader runs.

Public API: none. Wire: none.
