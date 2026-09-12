# [L] Measure and reduce CMAF remux work in HLS

## Goal

Reduce measured CMAF-to-CMAF HLS export work by preserving existing fragments
where their semantics match the requested segment. Keep public APIs, wire
formats, supported input, and HLS behavior unchanged.

## Plan

fMP4 import already publishes per-track CMAF fragments. `Muxer::init` preserves
the catalog init with track-id normalization, but `Muxer::read` decodes fetched
groups into media samples and HLS subsequently encodes fragments again. Measure
that remaining work before choosing an internal passthrough path.

Determine which valid fragment layouts can preserve payload ranges while
normalizing the output's track identity and sequence. Account for data offsets
and all affected boxes rather than assuming only `tfdt`, `mfhd`, and track id
need attention. Preserve native timescale, decode and presentation timestamps,
sample durations and flags, composition offsets, multiple fragments per group,
and HLS segment boundaries and duration reporting. Keep the existing sample
path for Legacy/LOC and valid inputs outside the optimization's eligibility.
Malformed or unsupported data must retain existing errors; never recover from
validation errors by switching paths or silently dropping data.

Compare equivalent HLS workloads before and after on audio/video segments using
`rs/moq-mux/src/container/fmp4/test_data/bbb.mp4` and generated boundary cases.
Wire semantic sample/timing equivalence, init consistency, multi-fragment groups,
and malformed-input checks into existing mux/HLS CI tests. Container bytes need
not be identical to ffmpeg output. Follow the measurement and no-win completion
rules in the [questline](/quest/m2/mux-copies/README.md).

Request deduplication and downstream moq.pro fan-out are outside this quest.

## Related

- [Fragment encoding](/quest/m2/mux-copies/fmp4-encode.md) - the remux path this may bypass
- [fMP4 import](/quest/m2/mux-copies/fmp4.md) - source-fragment ownership
- [CMAF copies](/quest/m2/cmaf-copy-budget.md) - browser implementation
