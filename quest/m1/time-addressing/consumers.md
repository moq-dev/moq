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
  too. moq-c's `group_duration_us` hint
  ([#7068](https://github.com/moq-dev/moq/pull/7068)), which declares the `hls`
  duration today, then declares the interval. A broadcast without one is not HLS-renderable.
- **Each group belongs to one segment, numbered by grid line**: the first
  line it contains, so an edge finds segment N from a `Time` start at pts
  N × interval, the group that resolves to. A group containing no line
  joins the segment of the group before it, or after a gap, the segment of
  the line it starts after. The edge trims each segment to its own groups,
  so ranges need no exclusive time end. The media sequence number is N.
- **Whole groups only.** Never split a group: a segment starting mid-GOP is
  not independent.
- **A segment with no groups is `EXT-X-GAP`**, so numbering never skips
  across a pause or an overrun. A group overrunning the interval is listed
  at its first line, so its `EXTINF` understates the media; document the
  deviation beside
  [fixed target duration](/quest/m1/archive/hls-target.md)'s overrun rule,
  whose target then comes from the interval.
- **The target duration is the interval** (maintainer, 2026-10-28). A
  segment can run up to twice the interval even without an overrun, since
  the group containing a line can start up to one interval before it;
  regular GOPs never do. One whose rounded `EXTINF` exceeds the target is
  listed under the overrun rule. Rejected: twice the interval, which is
  RFC 8216 compliant but doubles every player's hold-back.
- DASH lists the same segments by `$Time$`.

Pin the segmentation with table-driven tests over pauses that end between
lines or past the next one, unequal GOPs, and overruns.

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
