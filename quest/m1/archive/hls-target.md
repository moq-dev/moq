# [S] Fixed HLS target duration

## Goal

Every `moq-hls` media playlist of a broadcast advertises one
`EXT-X-TARGETDURATION` for the whole run, the largest declared duration across
its renditions' opt-in `hls` fields, so every edge, every reload, and every
rendition agree on it (Apple's HLS authoring spec 8.2). Segments stay one-to-one
with `hls` records: the edge never skips, splits, or renumbers one.

## Plan

[#4280](https://github.com/moq-dev/moq/pull/4280) derives the target from the
current window's longest segment (`snapshot_from_window` in
`rs/moq-hls/src/export/rendition.rs`), so it rises when a long record arrives
and falls when that record is evicted. Codex flagged it
([r4113921580](https://github.com/moq-dev/moq/pull/4280#discussion_r4113921580)).
The 09-28 audit reversed #4280's decision 4 (the observed maximum), and the
09-29 planning settled the rest:

- The target is `ceil` of the largest declared `hls` duration across the
  broadcast's renditions, from Catalog HLS field (retargeted 2026-10-17 from
  the root `archive` map; made broadcast-wide 2026-10-26, since renditions now
  cut on their own grids and may declare different durations). Until a
  rendition declares, it has no playlist yet. Decide what a rendition added
  later with a larger declared duration does to a target already served.
- A segment whose rounded `EXTINF` exceeds the target (a GOP that overran the
  publisher's declared value) is listed anyway, with a rate-limited warning.
  This deliberately deviates from RFC 8216 4.3.3.1's MUST, because refusing
  the segment loses content, and splitting or skipping it at the edge breaks
  the stable media sequence numbers of RFC 8216 6.2.2. Document the
  deviation. hls.js tolerates it; Apple's mediastreamvalidator flags it.

Catalog HLS field lists each rendition's own records, so the old
non-reference `EXTINF` rule and the split-record handling are already gone.

Replace `target_duration_follows_the_observed_segments` with tests that pin
the declared target across a window whose segment durations vary, and that
cover an overrun at the rounding boundary (listed, warned). Hold the target per
playlist URI, not per `Rendition`: `renditions::Producer::sync` replaces the
`Rendition` on a catalog reconfigure, so test that a reconfigure mid-run
keeps the original target.

