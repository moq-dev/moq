# [L] HLS export cuts segments on a declared grid, without records

## Goal

hang drops the per-rendition `hls` catalog field, the `<track>.hls` record
tracks, and A/V segment alignment. The catalog gains a `media` section with
`media.join.interval`, and the root `clock` moves to `media.clock` in the same
break. moq-mux, moq-c, and `@moq/publish` declare the interval. The HLS/DASH
export and the HLS recorder read the media tracks directly and number segments
on that grid from each group's first-frame pts. An HLS-only viewer of an
on-demand publisher starts its capture and gets a playlist.

## Plan

Decided 2026-11-02 (maintainer): the removal moves to m0, decoupled from
[Consumers by start](/quest/m1/time-addressing/consumers.md). That quest had
planned it (2026-10-27) to follow the time-addressing chain, cutting segments
from group start timestamps. Here the export uses each group's first-frame
pts instead, so nothing waits on groups by start. Once groups are addressed by
start, consumers.md swaps the first-frame pts for the group start.

- **Catalog.** A `media` section holds what applies to every audio and video
  track. `media.join.interval` promises that each one starts a group at least
  that often outside a pause, and is the export's grid, so every rendition
  shares segment numbers. It is fixed for the broadcast instance, since the
  numbering depends on it. Nested so a later join mechanism that is not a
  fixed grid can sit beside it. moq-mux declares it from its GOP hint, moq-c
  from `group_duration_us` ([moq-dev/moq#7068](https://github.com/moq-dev/moq/pull/7068)),
  which declares the `hls` duration today, and `@moq/publish` from its GOP
  setting. A broadcast without an interval is not HLS-renderable.
- **Clean break (maintainer, 2026-11-02).** No record fallback in the export,
  and no records emitted for old exports. `hang::hls`, the `hls` field in Rust
  and the JS zod schema, `<track>.hls` tracks, and A/V segment alignment are
  deleted; `moq_mux::hls` shrinks to declaring the interval.
- **Segmentation (maintainer, 2026-10-27/28, carried over unchanged).** HLS
  numbers segments by position, so every edge and every reload must derive the
  same consecutive numbers from timestamps alone; a pts grid is the only
  stateless rule that does. Rejected: one platform grid (long-GOP publishers
  overrun it constantly), per-edge counting (failover renumbers), and a
  publisher counter (per-producer state again).
  - Each group belongs to one segment, numbered by grid line: the first line
    it contains, so segment N starts at the group containing pts
    N × interval. A group spans from its first-frame pts to the next group's.
    A group containing no line belongs to the segment of the line it starts
    after, floor(pts / interval): the grid slots an overrun or a pause skips
    count as gaps (maintainer, 2026-11-03). On a 2s grid, A = [0s, 7s) is
    segment 0, slots 1 and 2 are gaps, and a contiguous B = [7s, 7.2s) starts
    segment 3. An edge joining at B derives its number from B's and its
    successor's timestamps alone. Rejected: B joining A's segment 0, which an
    edge joining at B cannot derive without older groups. The edge trims each
    segment to its own groups. The media sequence number is N.
  - Whole groups only. Never split a group: a segment starting mid-GOP is not
    independent.
  - A segment with no groups is `EXT-X-GAP`, so numbering never skips across a
    pause or an overrun. A group overrunning the interval is listed at its
    first line, so its `EXTINF` understates the media; document the deviation
    beside the fixed target duration's overrun rule in
    `rs/moq-hls/src/export/rendition.rs`.
  - The target duration is the interval (2026-10-28). A segment can run up to
    twice the interval even without an overrun, since the group containing a
    line can start up to one interval before it; regular GOPs never do. One
    whose rounded `EXTINF` exceeds the target is listed under the overrun
    rule. Rejected: twice the interval, which doubles every player's
    hold-back.
  - DASH lists the same segments by `$Time$`.
- Pin the segmentation with table-driven tests over pauses that end between
  lines or past the next one, unequal GOPs, and overruns.
- **On-demand capture (maintainer, 2026-11-02, moved from consumers.md).**
  The exporter requests record tracks today, but on-demand capture starts from
  media-track demand, so an HLS-only viewer never starts it. Prove with a test
  that an HLS-only viewer of an on-demand publisher (a default
  `moq import capture`, whose audio has been on demand since
  [moq-dev/moq#7238](https://github.com/moq-dev/moq/pull/7238)) starts capture
  and gets a playlist with both renditions.
- Convert the nightly segment conformance test
  (`rs/moq-hls/tests/integration/conformance.rs`, `just rs hls-conformance`),
  which declares its segment duration with `with_hls`. Extend it to check that
  every rendition of its unaligned ladder shares the grid's segment numbers
  while its segment boundaries follow its own keyframes (moved here from the
  conformance quest when that test landed, 2026-11-02).
- Convert the HLS recorder (`rs/moq-cli/src/hls/record.rs`). Update the hang
  draft and the HLS, CLI, and archive docs this makes stale, including
  `doc/concept/archives.md`, whose HLS paragraph says a recording needs its
  `hls` record tracks.
- **Open: an unhinted broadcast's interval.** moq-mux declares the interval
  from its GOP hint, but RTMP, SRT, and file or pipe imports (`moq import` of
  fMP4, TS, MKV, or FLV) carry none. Today their first segment declares the
  `hls` duration, at least the 4s default grid. Options: declare the 4s
  default for an unhinted broadcast at once, its GOPs packing or overrunning
  (recommended, keeps ingest HLS-renderable with today's segment length);
  declare the first complete video group, measured once; or leave an
  unhinted broadcast not HLS-renderable, as the Catalog bullet reads.
  Implementation waits on this answer.

Public API: breaking. `hang::hls` and the per-rendition `hls` field are
deleted, `clock` moves to `media.clock`, and `moq_mux::hls` shrinks to
declaring the interval. Wire: hang drops the `hls` field and the record
tracks, adds `media.join.interval`, and moves `clock` to `media.clock`.

Decided 2026-11-02 (maintainer, /quest-unblock), a follow-up of
[moq-dev/moq#7387](https://github.com/moq-dev/moq/pull/7387): the grid export's
media playlists advertise `EXT-X-INDEPENDENT-SEGMENTS` whenever every listed
segment opens on a sync sample. No renderer emits it today, and
[export-sync-flags](/quest/m2/intra-refresh/export-sync-flags.md) assumes it, so
that quest only has to omit it for warmup renditions. #7387's conformance probe
then checks the tag against each segment's first sample.

## Related

- [Consumers by start](/quest/m1/time-addressing/consumers.md) - later swaps the first-frame pts for group starts
- [HLS export follows a resolution change](/quest/m1/hls-reinit-same-codec.md) - its hand-over race is reconsidered without records
