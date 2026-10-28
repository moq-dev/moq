# [L] Media consumers address groups by start

## Goal

Every first-party consumer that numbered groups works by start timestamp,
in Rust and JS, and hang stops writing duration and discontinuity markers
that group ends and gaps now carry. The HLS/DASH export cuts segments
from group starts and ends on the broadcast's declared join interval, so the
HLS records go away.

## Plan

Decided 2026-10-25 (maintainer), from the spike:

- GROUP_END replaces hang's empty-frame duration marker
  (`rs/moq-mux/src/container/producer.rs:523`), so moq-mux stops writing
  empty frames.
- Discontinuity marker groups go away (`producer.rs:650`): a gap is a
  previous end below the next start, and a missing group is a
  previous-start link that doesn't match.
- Legacy egress synthesizes neither: old players lose end durations and
  pause markers, which they already tolerate being absent.

Decided 2026-10-27 (maintainer): moq-mux publishes HLS records by default
until this lands. Then the `<track>.hls` record tracks, the per-rendition
`hls` catalog field, and any A/V segment alignment go away.

Decided 2026-10-27 (maintainer, from an HLS audit): how the export
cuts segments without records. HLS numbers segments by position, so every
edge and every reload must derive the same consecutive numbers from
timestamps alone, and a pts grid is the only stateless rule that does.
Rejected: one platform grid (long-GOP publishers overrun it constantly),
per-edge counting (failover renumbers), and a publisher counter (per-producer
state again).

- **A catalog `media` section** holds what applies to every audio and video
  track. `media.join.interval` promises that each one starts a group at least
  that often outside a pause, and is the export's grid, so every rendition
  shares segment numbers. It is fixed for the broadcast instance, since the
  numbering depends on it. The root `clock` moves to `media.clock` in the same
  break. Nested so a later join mechanism that is not a fixed grid can sit
  beside it.
  moq-mux declares the interval from its GOP hint, and `@moq/publish` does
  too. A broadcast without one is not HLS-renderable.
- **Segment N starts at the group containing pts N × interval**, the group a
  `Time` start at that pts resolves to, so an edge serves a segment from its
  number alone. It runs up to where segment N + 1 starts, which the edge
  trims itself; ranges need no exclusive time end. The media sequence number
  is N.
- **Whole groups only.** Never split a group: a segment starting mid-GOP is
  not independent.
- **A line inside a pause or gap** starts its segment at the first group
  after it, so media resuming between lines is never dropped. If none starts
  before the next line, the segment is `EXT-X-GAP`.
- **A group containing more than one line** (it overran the interval) is
  listed at its first line, and each line it swallows is an `EXT-X-GAP`
  segment, so numbering never skips. Its `EXTINF` understates the media;
  document the deviation beside
  [fixed target duration](/quest/m1/archive/hls-target.md)'s overrun rule,
  whose target then comes from the interval.
- **Segments run under twice the interval** even without an overrun, since
  the group containing a line can start up to one interval before it. Open
  for the maintainer: a target of twice the interval (RFC 8216 compliant,
  but players hold back twice as far) or the interval itself with longer
  segments listed under the overrun rule.
- DASH lists the same segments by `$Time$`.

Pin the segmentation with table-driven tests over pauses that end between
lines, unequal GOPs, and overruns.

Convert each consumer that numbers groups: hang and moq-mux timelines,
HLS/DASH export, `seek` for GOP-aligned failover in moq-mux, HLS import's
discontinuity packing, transcode's 1:1 numbering, archive keys and resume,
relay and CLI `fetch --group`, and player continuity checks in the Rust and
JS container consumers and `js/watch`. Update the hang draft and the docs
each one makes stale; rename CLI flags per the repo's CLI rule.

Added 2026-10-27 (maintainer, from an external release-to-main audit): an
HLS exporter today requests the record tracks, but on-demand capture starts
from media-track demand, so an HLS-only viewer never starts it. Once the
export reads media tracks directly, prove that an HLS-only viewer of an
on-demand publisher starts capture and gets a playlist.

Public API: breaking where a consumer exposes group numbers (CLI
`--group`, archive keys), `hang::hls` and the per-rendition `hls` field are
deleted, and `moq_mux::hls` shrinks to declaring the interval. Wire: hang
drops empty duration frames, discontinuity marker groups, the `hls` field,
and the record tracks, adds `media.join.interval`, and moves `clock` to
`media.clock`.

## Required

- [Rust groups by start](/quest/m1/time-addressing/model.md) - the Rust API these consume
- [@moq/net by start](/quest/m1/time-addressing/js.md) - the JS API these consume
