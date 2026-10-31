# [M] HLS export follows a resolution change under the same codec string

## Goal

When a rendition changes resolution (or any other decoder-affecting field) while keeping its codec string, the HLS and DASH export starts a new init segment and segment tag for it, so every served segment decodes under the init it is listed with, the rendition's first segments are listed, and its recording cursor keeps up.

## Plan

Found by the conformance test in [moq-dev/moq#7387](https://github.com/moq-dev/moq/pull/7387): after `video1` switches from 320x180 to 480x270 with the same codec string, the export keeps the old segment tag and init. The 480x270 segments are served under the 320x180 init with no in-band parameter sets, so they don't decode, that rendition's first segments are never listed, and its recording cursor fails with `RecordingLagged`. When the codec string also changes, the importer makes a new rendition name and everything passes.

Decided 2026-10-30 (maintainer, follow-up planning after the /quest-afk run). Treat a change of the rendition's decoder identity (the same fields hang's `same_identity` compares) as a new instance in the export, as a codec-string change already is. Reuse the existing instance-replacement path rather than adding a second one. The conformance test's `video1` case is the regression test; it must pass with the same-codec switch restored.

Investigated 2026-10-31. The original failure is a fixture artifact. The generated transport stream puts `video1`'s 12-second size switch before the first packets of `video0` and audio. Their catalog reservations withhold the initial snapshot until `video1` already declares 480x270, so the export never sees a 320x180 catalog to replace. The export's predicate already compares the normalized full config, dimensions included (`matches_video` in `rs/moq-hls/src/export/rendition.rs`), so no predicate change is needed. (hang's `same_identity` excludes coded dimensions, so it is the wrong boundary for the export anyway.)

Decided 2026-10-31 (maintainer): fix the fixture first. The fixture lives in [moq-dev/moq#7387](https://github.com/moq-dev/moq/pull/7387). Muxing it with `-max_interleave_delta 0` repairs the interleaving. The import then fails at the switch with `catalog jitter cannot decrease for a published rendition`, which is planned separately.

Rerun 2026-10-31, with the repaired fixture and a local workaround for the jitter failure: the export now replaces the instance with a new init and segment tag, every served `video1` segment decodes under its init, and the recording cursor keeps up. Two failures remain, both in the instance handoff rather than the predicate:

- Group 4 (pts 13.466 s, the first 480x270 group) reaches the export's records watcher before the catalog update. The old instance claims it as pending, so the handoff starts the replacement at 5. Group 4 is listed only by the retired 320x180 instance, which nothing serves. Arrival order across the catalog and record tracks decides the handoff point.
- `Renditions::sync` (`rs/moq-hls/src/export/renditions.rs`) drops the retired instance from the playlist URI at once, and the replacement's window starts empty at the handoff. Segments 3 and 4 are never listed: the playlist jumps from 2 to 5. The conformance test reports `evicted segments 3 to 5 unseen`, `skipped a number`, a continuity break from 10.47 s to 16.47 s, and `recorded [0..7] but listed [0, 1, 2, 5, 6, 7]`. RFC 8216 section 6.2.2 does not allow removing those segments yet.

So the instance-replacement predicate change does not suffice. Re-scope this quest to the handoff before implementing it: the replacement lists the switching group under the new init, and the playlist keeps the predecessor's windowed segments, with their own `EXT-X-MAP`, behind an `EXT-X-DISCONTINUITY`. Keep final AAC segment duration and B-frame decode times separate.

Public API: none. Wire: none.

## Related

- [HLS and DASH segment conformance](/quest/m1/hls-conformance.md) - the test that found it, which waits on this
