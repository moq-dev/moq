# [M] HLS and DASH segment conformance, nightly

## Goal

A nightly test serves a multi-rendition broadcast (video renditions and
audio) through `moq-hls`, probes every listed segment with ffprobe, and
fails if what the manifests advertise disagrees with the media served:
`EXTINF` and DASH `t`/`d` against each segment's decoded start and duration,
`EXT-X-TARGETDURATION` held for the run, sync flags against `startWithSAP` and
`EXT-X-INDEPENDENT-SEGMENTS`, and the same URL returning the same bytes across
reloads and a mid-run catalog estimate update.

## Plan

From the 2026-10-24 moq-hls audit, which found no player-level HLS or DASH
coverage. Decided with the maintainer on 2026-10-24:

- ffprobe conformance lands in m1, so it validates the
  Catalog HLS field redesign and the fixed target duration before the cut. A
  browser player test follows in m2
  ([HLS and DASH browser playback](/quest/m2/hls-player-test.md)).
- Cover a sliding window that evicts segments, a GOP that overruns the
  declared duration, a mid-run reconfiguration (new rendition instance), and a
  recording through the segment cursors.
- Cover an unaligned ladder, renditions whose keyframes and segment numbers
  differ: every segment is listed and fetched (no `EXT-X-GAP`), and the DASH
  manifest omits `segmentAlignment`.
- Runs nightly; move any part fast enough into the merge gate.

Public API: none. Wire: none.

Found 2026-10-30, by the test in `rs/moq-hls/tests/integration/conformance.rs`
(`just rs hls-conformance`), which passes everything else:

- An H.264 resolution change that keeps the codec string (one level for both
  halves) through the TS import breaks the export. The replacement instance
  reuses the old segment tag and the cached init, so the 480x270 segments are
  served under the 320x180 init, which has no in-band parameter sets, and fail
  to decode. The rendition's first segments are never listed, and its
  recording cursor fails with `RecordingLagged`. A change of codec string mints
  a new track name instead and conforms.
- The final segment of a TS-imported AAC track is one frame short (`EXTINF`
  and `S@d` 1.365 against 1.387 decoded): its stop record lands at the last
  frame's start. The producer alone records the end correctly
  (`Producer::finish` with no buffering), so the loss is in the TS import path.

The test lands in the nightly once both conform, or with them excluded; see
the PR's decisions.

## Related

- [Export sync flags](/quest/m2/intra-refresh/export-sync-flags.md) - the sync flags a refresh rendition should advertise
