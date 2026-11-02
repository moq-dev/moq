# [M] HLS export follows a resolution change under the same codec string

## Goal

When a rendition changes resolution (or any other decoder-affecting field) while keeping its codec string, the HLS and DASH export starts a new init segment and segment tag for it, so every served segment decodes under the init it is listed with, the rendition's first segments are listed, and its recording cursor keeps up.

## Plan

Found by the conformance test in [moq-dev/moq#7387](https://github.com/moq-dev/moq/pull/7387): after `video1` switches from 320x180 to 480x270 with the same codec string, the export keeps the old segment tag and init. The 480x270 segments are served under the 320x180 init with no in-band parameter sets, so they don't decode, that rendition's first segments are never listed, and its recording cursor fails with `RecordingLagged`. When the codec string also changes, the importer makes a new rendition name and everything passes.

Decided 2026-10-30 (maintainer, follow-up planning after the /quest-afk run). Treat a change of the rendition's decoder identity (the same fields hang's `same_identity` compares) as a new instance in the export, as a codec-string change already is. Reuse the existing instance-replacement path rather than adding a second one. The conformance test's `video1` case is the regression test; it must pass with the same-codec switch restored.

Public API: none. Wire: none.

## Related

- [HLS and DASH segment conformance](/quest/m1/hls-conformance.md) - the test that found it, which waits on this
