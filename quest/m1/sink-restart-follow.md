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

Decided with the maintainer on 2026-10-24, when the play, moqsrc, and
transcode moves landed:

- The TS follower follows `Instances`, with `--follow` bounding only the wait
  for a publisher after an end.
- `export hls` gives each instance its own playlist URL. While the old
  publisher stays live, `Server::broadcaster` (`rs/moq-hls/src/server/mod.rs`)
  answers new requests from its cached broadcaster, so an overlapping
  replacement never reaches them; swapping the cache alone would reset the
  source-derived `EXT-X-MEDIA-SEQUENCE` at a stable URL. Rejected: one
  continuous playlist with monotonic media and discontinuity sequences. A
  failed timeline keeps answering its playlists with a no-store 5xx.
- `export rtmp` and `export rtc`, both `--listen` and `--connect`, stay on the
  old instance until its route dies, never silently resetting timestamps or
  codec state. Rejected for now: reconnecting the output per instance, and
  marking the break in-session.

Remaining, each with a test that replaces the publisher while the reader runs:

- `export hls`: a playlist URL per instance.
- `export rtmp` and `export rtc`: confirm both sides stay pinned and fail once
  the old route dies.

Public API: none. Wire: none.

## Related

- [Finite drain for a replaced run's HLS recording](/quest/m2/hls-restart-drain-cutoff.md) - the recording side of an HLS restart
