# [S] OBS defaults to capped VBR

## Goal

The OBS plugin's dock profiles configure the user's encoder for capped VBR
(constant quality with the profile's bitrate as the peak) instead of CBR, the
catalog `bitrate` carries that peak for a VBR encoder instead of the measured
running maximum, and dynamic bitrate moves the peak.

## Plan

The epic's decisions apply: quality mode with a peak where honored, otherwise
CBR.

Today `cpp/obs/src/moq-dock.cpp` sets `rate_control = "CBR"` with
2500 kbps, or 8000/2500 for the Quality/Performance profiles, and
`moq-output.cpp` supplies a catalog bitrate only for CBR, so a VBR stream
advertises its measured peak, which starts low on a calm opening scene.
Candidates: x264's `VBR` (CRF under a VBV max rate) and NVENC's `VBR` or `CQVBR`
with `max_bitrate`; check each encoder's settings keys and the
`OBS_ENCODER_CAP_DYN_BITRATE` path. The profile bitrates follow the
[cap formula](/quest/m1/video-rate/cap-formula.md) for their resolution.

Public API: none. Wire: none.

## Required

- [One default cap formula](/quest/m1/video-rate/cap-formula.md) - the peaks the profiles use
