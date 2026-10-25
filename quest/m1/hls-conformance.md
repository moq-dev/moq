# [M] HLS and DASH segment conformance, nightly

## Goal

A nightly test serves an aligned multi-rendition broadcast (video renditions
and audio) through `moq-hls`, probes every listed segment with ffprobe, and
fails if what the manifests advertise disagrees with the media served:
`EXTINF` and DASH `t`/`d` against each segment's decoded start and duration,
`EXT-X-TARGETDURATION` held for the run, sync flags against `startWithSAP` and
`EXT-X-INDEPENDENT-SEGMENTS`, and the same URL returning the same bytes across
reloads and a mid-run catalog estimate update.

## Plan

From the 2026-10-24 moq-hls audit, which found no player-level HLS or DASH
coverage. Decided with the maintainer on 2026-10-24:

- ffprobe conformance lands in m1, so it validates the
  [Catalog HLS field](/quest/m1/archive/catalog-hls.md) redesign and the
  [fixed target duration](/quest/m1/archive/hls-target.md) before the cut. A
  browser player test follows in m2
  ([HLS and DASH browser playback](/quest/m2/hls-player-test.md)).
- Cover a sliding window that evicts segments, a GOP that overruns the
  declared duration, a mid-run reconfiguration (new rendition instance), and a
  recording through the segment cursors.
- Runs nightly; move any part fast enough into the merge gate.

Public API: none. Wire: none.

## Required

- [Catalog HLS field](/quest/m1/archive/catalog-hls.md) - aligned per-rendition records that this checks the manifests against
- [Fixed HLS target duration](/quest/m1/archive/hls-target.md) - the run-wide target this asserts

## Related

- [Export sync flags](/quest/m2/intra-refresh/export-sync-flags.md) - the sync flags a refresh rendition should advertise
