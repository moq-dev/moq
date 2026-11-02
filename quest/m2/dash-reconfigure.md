# [M] The DASH manifest keeps segments across a reconfigure

## Goal

After a same-codec reconfigure (such as a resolution change) replaces a
rendition's instance, the DASH manifest keeps listing the old instance's
segments, each under the init it was muxed for, until the window slides past
them, as the HLS media playlist does. A DASH player loses no listed segment
at the switch.

## Plan

Decided 2026-10-31 (maintainer, the /quest-complete requeue session), a
follow-up of [moq-dev/moq#7436](https://github.com/moq-dev/moq/pull/7436).
That PR keeps a replaced instance (`Rendition::previous`) and lists its
segments in the HLS playlist behind `EXT-X-DISCONTINUITY`, each with its own
`EXT-X-MAP`. `Rendition::segment` and `Rendition::init` already resolve a
predecessor's URLs. The DASH manifest (`rs/moq-hls/src/export/mpd.rs`,
`render_manifest`) still renders one `Period id="0"` with only the current
instance's segments, and `doc/bin/hls.md` says so.

Represent the predecessor in DASH by either:

- **multiple Periods**, one per instance, each with its own
  `SegmentTemplate` and initialization URL; or
- **a per-segment init** in one Period.

Choose during implementation by which dash.js and the conformance test
accept without a reload, and record the choice in the PR. Reuse the
predecessor window #7436 adds rather than tracking a second one, and update
`doc/bin/hls.md`. Test that a reconfigure keeps the old segments listed in
the manifest, and extend the DASH checks of
[HLS and DASH segment conformance](/quest/m1/hls-conformance.md) if it has
landed.

Public API: none. Wire: none.

## Required

- [HLS export follows a resolution change](/quest/m1/hls-reinit-same-codec.md) - the instance hand-over (#7436) this extends to DASH

## Related

- [HLS and DASH browser playback](/quest/m2/hls-player-test.md) - dash.js survives reconfiguration nightly
