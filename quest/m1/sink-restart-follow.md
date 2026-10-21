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
to other broadcasts stays with
[catalog references](/quest/m0/broadcast-epoch/catalog-references.md).

Readers with their own follow loop today move onto `announced_broadcast`:

- moqsrc: `follow_path` in `rs/moq-gst/src/source/imp.rs` drains
  `origin.follow` and re-requests on each start, holding its pads across
  instances.
- `moq play`: `play` in `rs/moq-cli/src/play/media.rs` does the same and
  resets its presentation per instance.
- The TS stitching follower (`rs/moq-mux/src/container/ts/follower.rs`), if
  `announced_broadcast` fits how it marks a break.

`subscribe` reads until its track ends, so it follows like the rest. Only a
finite read such as `fetch` stays on the one instance it resolved.

The stdout exports already do this: TS follows with `--stitch`, and the other
formats ride the old instance until its route dies and its tracks error. Audit
and fix the rest the same way:

- `export hls`: `moq-hls`'s server resolves per request; check that a restart
  moves new requests to the new instance and how a playlist marks the break
  (`EXT-X-DISCONTINUITY`); a failed timeline answers its playlists with a no-store 5xx.
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
rather than one rule per reader, built on `announced_broadcast`. Each fix
lands with a test that replaces the publisher while the reader runs.

Public API: possibly a shared follow helper in `moq-mux`. Wire: none.
