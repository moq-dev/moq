# [M] Capped VBR on MediaFoundation

## Goal

`moq-video`'s MediaFoundation backend encodes constant quality under a hard
peak at the rendition's cap where the hardware MFT supports it, or CBR at the
cap otherwise, and reports which it got instead of only logging a refused
knob. The PR measures the average against the cap on static, talking head,
and high-motion clips.

## Plan

The epic's decisions apply: quality mode with a peak where honored, decided at
setup, otherwise CBR at the cap; bitrate `min(cap, grant)`.

Today `rs/moq-video/src/encode/backend/mediafoundation.rs` sets
`eAVEncCommonRateControlMode_CBR` and logs refusals at debug level. Candidates:
`eAVEncCommonRateControlMode_Quality` or `QualityBasedVBR` with
`AVEncCommonMaxBitRate` and `AVEncCommonBufferSize`, probed at setup; a
refusal falls back to CBR, and a refused CBR fails the open.

Public API: none. Wire: none.

## Hosts

- windows - MediaFoundation runs only on Windows
