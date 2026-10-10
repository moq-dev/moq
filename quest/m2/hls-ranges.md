# [M] Range-addressed HLS playlists

## Goal

`moq-hls` serves a playlist addressed by a start and end over a durable
(archived) timeline: it lists only the segments covering that range, and
ends with `EXT-X-ENDLIST` once the range is fully in the past, so a player
gets a VOD clip of a recording without the embedder building its own
playlist. Today `moq_hls::export::Config` (`rs/moq-hls/src/export/mod.rs`)
has only `window` and `history`, so a playlist is either the live window or
everything stored.

The consumer is moq.pro's managed HLS, which Requires this quest.

## Plan

Decided 2026-10-08: range playlists belong upstream in `moq-hls`, in m2, so
moq.pro builds on the stock exporter instead of a fork of its playlist code.

Open, to settle in the PR with a recommendation:

- How a request names the range. Recommended: `start` and `end` on the
  playlist URL in the timeline's media time, so one exporter serves any
  number of clips and an edge cache keys on the URL.
- Which edges a range snaps to. Recommended: the segments that overlap it,
  with the start widened back to the preceding sync point (or refused when
  none is held), since a long GOP spans several segments. Test over the
  existing split-GOP fixture in `rs/moq-hls/src/export/mod.rs`.
- What a range over a timeline that is not durable, or outside what the
  store holds, returns. Refuse it with a clear HTTP error rather than list a
  partial range.

A range that ends in the future grows like an event playlist until its end
passes. Every edge must list the same segments and `EXT-X-MEDIA-SEQUENCE` for the
same range, regardless of when it joined. `Config::history` starts at the
records restated when an exporter joins, so this relies on
[replay history](/quest/m1/archive/replay-history.md); test two edges that
join at different times.

Tests: a recording longer than the default window, clipped at its start, in
its middle, and across the live edge; a range on a live-only timeline is
refused. Document it in `doc/bin/hls.md`.

Public API: `moq-hls` export gains range addressing. Wire: none.

## Required

- [History from the start](/quest/m1/archive/replay-history.md) - reads stored timeline groups, which a range older than the restated records needs
