# [M] HLS export follows a resolution change under the same codec string

## Goal

When a rendition changes resolution (or any other decoder-affecting field) while keeping its codec string, the HLS and DASH export starts a new init segment and segment tag for it, so every served segment decodes under the init it is listed with, the rendition's first segments are listed, and its recording cursor keeps up.

## Plan

Found by the conformance test in [moq-dev/moq#7387](https://github.com/moq-dev/moq/pull/7387): after `video1` switches from 320x180 to 480x270 with the same codec string, the export keeps the old segment tag and init. The 480x270 segments are served under the 320x180 init with no in-band parameter sets, so they don't decode, that rendition's first segments are never listed, and its recording cursor fails with `RecordingLagged`. When the codec string also changes, the importer makes a new rendition name and everything passes.

Decided 2026-10-30 (maintainer, follow-up planning after the /quest-afk run). Treat a change of the rendition's decoder identity (the same fields hang's `same_identity` compares) as a new instance in the export, as a codec-string change already is. Reuse the existing instance-replacement path rather than adding a second one. The conformance test's `video1` case is the regression test; it must pass with the same-codec switch restored.

Implementation investigation 2026-10-31: the original conformance failure reproduces, but
its proposed cause does not match the current source. The export already compares the
normalized full video/audio config, including dimensions, description, container, and
HLS record track, and retires/replaces on a change. Its existing
`decoder_config_change_rebuilds_the_rendition` and
`a_reconfigure_versions_the_init_url` tests cover that path.

The generated transport stream puts `video1`'s 12-second size switch before the first
packets of `video0` and audio. Their outstanding catalog reservations withhold the
initial snapshot until `video1` already declares 480x270. The export therefore never
receives a 320x180 catalog to replace; it bootstraps its cached init from older retained
320x180 media and later serves 480x270 media under it. Fixing the fixture's interleaving
with ffmpeg's `-max_interleave_delta 0` instead reaches an importer error at the switch:
`catalog jitter cannot decrease for a published rendition`.

Replan before implementation: decide whether this quest should cover retained inline
media whose geometry changes before the first catalog, or whether the conformance
fixture should first be repaired and its newly exposed importer error fixed. Merely
changing the export's matching predicate cannot fix the reproduced failure. Also,
hang's current `same_identity` excludes coded dimensions, which are track ceilings;
use “export mux/decode configuration, including in-band geometry” if that is the
intended boundary. Keep final AAC segment duration and B-frame decode times separate.

Public API: none. Wire: none.

## Related

- [HLS and DASH segment conformance](/quest/m1/hls-conformance.md) - the test that found it, which waits on this
