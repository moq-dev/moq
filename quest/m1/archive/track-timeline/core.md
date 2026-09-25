# [XL] Rust per-track timelines

## Goal

The moq-hang draft, `hang`, `moq-mux`, `moq-archive`, and `moq-hls` use one
timeline per track, per the [line's decisions](/quest/m1/archive/track-timeline/README.md).
A DVR recording keeps every track's newest group, an append-only group is
stored as its frames arrive, and `moq-hls` serves aligned HLS and DASH from
live or recorded timelines without the publisher cutting for it.

## Plan

These crates compile together, so they change in one PR. Update the draft's
timeline and Recording sections in the same change, in a new draft revision;
most of their rules assume one aligned segment counter (one timeline per
broadcast, cross-track boundaries, whole-segment retention, reading segment N
of track T through record N).

- **Timeline:** a record describes one stored span of its own track: sequence,
  pts, duration, and the group and frame range. Drop cross-track pacing and the
  completeness wait; keep manual cuts. Retention pops each track's window
  independently and always keeps its newest record.
- **Catalog:** replace the root `archive.track` with a map from track to
  timeline, covering the catalog track too.
- **Writer and reader:** commit each track independently, and store frame
  ranges so a never-closing group is recorded. Bump the recording `version` and
  refuse the old one. Recovery, grace deletion, and FETCH replay follow the
  per-track index.
- **HLS and DASH:** take segment boundaries from a reference video rendition,
  near a target duration, and number them so every edge and every reload
  agrees, including after DVR pops. Every other video rendition snaps each
  boundary to its nearest group start within a tolerance (around 1s); a
  segment with no start in range becomes a gap (`EXT-X-GAP` in HLS), so a
  player switching renditions lands on the next real segment. Gaps are a
  best-effort fallback: the draft and HLS docs say a publisher wanting HLS
  export SHOULD align video GOPs across renditions. Audio and other
  renditions take the groups and frames whose timestamps fall in each span,
  possibly from more than one object. The reader's cache absorbs the overlap.

Carry over the existing tests (DVR trim, durable listing, archive replay) and
add a static catalog outliving its first video segment, an append-only group
spanning several objects, audio cut independently of video, renditions that
start at different times, and video renditions whose group starts never
coincide.

Update `doc/concept/hang.md` and the HLS and CLI docs this makes stale.
