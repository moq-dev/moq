# [M] Capped VBR on VideoToolbox

## Goal

`moq-video`'s VideoToolbox backend, used on macOS and iOS, encodes constant
quality under a hard peak at the rendition's cap, or CBR at the cap where
VideoToolbox does not honor that in real time. Today it sets only
`AverageBitRate`, which the encoder may exceed. The PR measures the average
against the cap on static, talking head, and high-motion clips.

## Plan

The epic's decisions apply: quality mode with a peak where honored, decided at
setup, otherwise CBR at the cap; bitrate `min(cap, grant)`.

Candidates in `rs/moq-video/src/encode/backend/videotoolbox.rs`:
`kVTCompressionPropertyKey_Quality` with `DataRateLimits` at the cap, and the
low-latency rate control encoder specification; `ConstantBitRate` as the
fallback. Verify by measurement that real-time sessions honor the quality key
and the limit, since the docs leave it unclear. Retuning at runtime must move
the limit along with the target.

Public API: none. Wire: none.

## Hosts

- macos - VideoToolbox runs only on Apple platforms
