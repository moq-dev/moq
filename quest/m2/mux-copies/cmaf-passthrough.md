# [L] Splice CMAF hang groups into HLS segments

## Goal

CMAF→CMAF HLS export does not demux+remux samples. CPU is at least 2×
lower and coded bytes are copied at most once. Sample payload and `tfdt`
match the remux path.

## Plan

`moq-hls` `Rendition::segment` FETCHes every group, `Muxer::read` parses
each moof+mdat into samples (`Bytes::copy_from_slice` per sample), then
`encode_fragment` concatenates payloads and encodes the moof twice. fMP4
ingest already published complete per-track fragments as hang frames.

For `Container::Cmaf`, splice/rewrite existing fragments (track id /
`mfhd` / `tfdt` only). Keep the sample path for Legacy/LOC.

Edge fan-out (N viewers × N transmux) is the downstream
moq.pro `quest/m2/perf/live-hls-singleflight` quest. This is the remux
inside one call.

Acceptance: Criterion `muxer.cmaf_passthrough` vs `muxer.sample_remux` on
`bbb.mp4` segments (1 s / 6 s, 1080p + audio). Hang roundtrip: import fMP4
→ HLS segment bytes vs `ffmpeg -c copy`. Byte-identical samples and `tfdt`.

## Related

- [fMP4 import and fragment encode](/quest/m2/mux-copies/fmp4.md) - the remux path this skips
- [CMAF copies](/quest/m2/cmaf-copy-budget.md) - JS
