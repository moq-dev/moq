# [S] OBS runs capped VBR on VideoToolbox and VA-API

## Goal

The OBS plugin's dock profiles run VideoToolbox (macOS) and VA-API encoders at
constant quality under the profile's peak, where that peak is measured to hold,
instead of the CBR they fall back to today.

## Plan

The epic's decisions apply: quality mode with a peak where honored, otherwise
CBR.

Done: `ApplyEncoderRate` in `cpp/obs/src/moq-encoder-rate.h` runs x264 as
`VBR` (CRF under a VBV max rate) and NVENC as `CQVBR` with `max_bitrate`, and
the output prices, advertises, and retunes that peak (NVENC under `max_bitrate`).
Quick Sync (ICQ has no max rate) and AMF (QVBR sets no peak) stay CBR for good.

Remaining, each needing hardware to measure that the peak holds:

- VideoToolbox: `rate_control` `CRF` (Apple Silicon only) with `limit_bitrate`,
  `max_bitrate` and `max_bitrate_window`, which becomes a `DataRateLimits` hint
  that may exceed the cap; an Intel Mac falls back to ABR. `vt_update` only
  re-applies on a `bitrate` change, so a retune sets `bitrate` and `max_bitrate`
  together.
- VA-API: `QVBR` where the driver lists it (`vaapi_device_rc_supported`), with
  `maxrate` as the peak; check what the driver does with `bitrate` and whether
  the encoder advertises `OBS_ENCODER_CAP_DYN_BITRATE` (it does not today).

Whatever measures as not holding stays CBR. Decided by the maintainer: "Use
OBS Output settings" in Simple mode stays CBR, and every encoder keeps its own
default quality level (x264 CRF 23, NVENC target quality 20) until the VMAF
harness ([quest/m9/video-quality](/quest/m9/video-quality/README.md)) picks one. Extend `EncoderPeakKey` and
`ApplyEncoderRate`; the output follows from them.

Public API: none. Wire: none.

## Hosts

- macos - VideoToolbox's rate limit can only be measured on a Mac
- linux - VA-API's quality mode depends on the driver, so it is measured on one
