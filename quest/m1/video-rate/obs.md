# [S] OBS runs capped VBR on VideoToolbox

## Goal

The OBS plugin's dock profiles run VideoToolbox (macOS) at constant quality
under the profile's peak, if that peak is measured to hold, instead of the
CBR it falls back to today.

## Plan

The epic's decisions apply: quality mode with a peak where honored, otherwise
CBR.

`EnforceCap` in `cpp/obs/src/moq-encoder-rate.h` owns the cap on every video
encoder the MoQ output starts. x264 and NVENC run constant quality under it;
Quick Sync, AMF, and VA-API stay CBR for good. VA-API's QVBR was measured past
the cap on Mesa radeonsi (Radeon 610M and RX 9070) and Intel iHD (Arrow Lake),
for H.264, HEVC, and AV1 ([moq-dev/moq#9193](https://github.com/moq-dev/moq/pull/9193)).

Remaining, which needs an Apple Silicon Mac to measure that the peak holds:

- VideoToolbox: `rate_control` `CRF` (Apple Silicon only) with `limit_bitrate`,
  `max_bitrate` and `max_bitrate_window`, which becomes a `DataRateLimits` hint
  that may exceed the cap; an Intel Mac falls back to ABR. `vt_update` only
  re-applies on a `bitrate` change, so `SetBitrate` must write `bitrate` beside
  `max_bitrate` and clamp it to the granted peak, as it does for NVENC's plain
  VBR (`MoQPeak::average_kbps`); `Reset()` restores both.
- Measure it the way VA-API was: encode the same 40 s clips (Big Buck Bunny,
  with heavy noise, light grain, a still frame, and a still frame that turns to
  noise) at a 3 Mbps cap in CBR and in CRF with the limit, and compare the worst
  1 s window of each against the cap and against CBR's own.

If it measures as not holding, it stays CBR. Decided by the maintainer: "Use
OBS Output settings" in Simple mode stays CBR, and every encoder keeps its own
default quality level (x264 CRF 23, NVENC target quality 20) until the VMAF
harness ([quest/m9/video-quality](/quest/m9/video-quality/README.md)) picks one. Extend `EnforceCap` and
`EncoderPeakKey` for the mode, and the retune and restore in `moq-output.cpp` for
the setting a grant writes.

Public API: none. Wire: none.

## Hosts

- macos - VideoToolbox's rate limit can only be measured on a Mac
