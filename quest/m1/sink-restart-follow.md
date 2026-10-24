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
to other broadcasts landed for `moqsrc` through `moq_mux::Source::follow`;
`moq play` resolves a reference once with `Source::resolve`, which waits for
a sibling's first announcement and keeps it.

`moq play`, `moqsrc`, and `transcode` follow through
`moq_mux::follow::Instances::new(origin, path, None)` (decided 2026-10-20,
maintainer: no reader hand-rolls its own follow loop). Each
`follow::Event::Start` builds a `moq_mux::Source` from the instance it hands
out, and a selected handler's refusal is terminal. `transcode` retires the old
ladder and publishes a fresh derivative broadcast with its own epoch for each
source instance; a retired ladder aborts its catalog and rungs with `Cancel`.

A finite read such as `fetch`, every stdout export except TS with `--stitch`,
and TS without `--stitch` stay pinned to the instance they resolved until its
route dies and its tracks error. Never concatenate independent instances into
one stream.

Remaining, each with a test that replaces the publisher while the reader runs:

- The TS follower (`rs/moq-mux/src/container/ts/follower.rs`), used by
  `export ts --stitch` and both SRT sides, moves onto `Instances`. Its
  `with_linger` wait covers announcement through resolution and the first
  catalog, while `Instances` yields `Start` only after resolution, so moving
  the timer onto `Start` would silently extend that bound.
  [TS export follows by cause](/quest/m1/broadcast-lifecycle/ts-follow.md)
  reworks the same wait (`--follow`, no catalog deadline); land this move with
  or after it so the follower's timers change once.
- `export hls`: while the old publisher stays live, `Server::broadcaster`
  (`rs/moq-hls/src/server/mod.rs`) answers new requests from its cached
  broadcaster, so an overlapping replacement never reaches them. Swapping the
  cache alone resets the source-derived `EXT-X-MEDIA-SEQUENCE` at a stable
  playlist URI. Open decision: one continuous playlist with monotonic media
  and discontinuity sequences, or a playlist URI per instance. A failed
  timeline keeps answering its playlists with a no-store 5xx.
- `export rtmp` and `export rtc`, both `--listen` and `--connect`: open
  decision whether a new instance reconnects the output or marks the break
  in-session. Until decided they stay pinned, never silently resetting
  timestamps or codec state.

Public API: none. Wire: none.
