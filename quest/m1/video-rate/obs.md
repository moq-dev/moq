# [S] OBS runs capped VBR on VideoToolbox and VA-API

## Goal

The OBS plugin's dock profiles run VideoToolbox (macOS) and VA-API encoders at
constant quality under the profile's peak, where that peak is measured to hold,
instead of the CBR they fall back to today.

## Plan

The epic's decisions apply: quality mode with a peak where honored, otherwise
CBR.

Done: the MoQ output owns the cap on every video encoder it starts (maintainer
decision on the PR). `EnforceCap` in `cpp/obs/src/moq-encoder-rate.h` writes it
before OBS initializes the encoder, replaces any conflicting user rate option
with a logged warning, and the catalog advertises exactly that cap: x264 keeps
CBR and otherwise runs `VBR` (CRF under a VBV whose max rate and buffer are the
bitrate; `x264opts` is never rewritten, and a rate option in it logs a warning that
recommends the dock profiles), NVENC keeps
CBR and VBR and otherwise runs `CQVBR` with `max_bitrate`, and every other
encoder runs CBR at the cap. The output prices and retunes that peak (NVENC
under `max_bitrate`). Quick Sync (ICQ has no max rate) and AMF (QVBR sets no
peak) stay CBR for good. An encoder already running when the output starts keeps its
settings and the catalog advertises the cap it runs with; one running uncapped (CQP, CRF,
lossless) refuses the output.

Remaining, each needing hardware to measure that the peak holds:

- VideoToolbox: `rate_control` `CRF` (Apple Silicon only) with `limit_bitrate`,
  `max_bitrate` and `max_bitrate_window`, which becomes a `DataRateLimits` hint
  that may exceed the cap; an Intel Mac falls back to ABR. `vt_update` only
  re-applies on a `bitrate` change, so `SetBitrate` must write `bitrate` beside
  `max_bitrate` and clamp it to the granted peak, as it does for NVENC's plain
  VBR (`MoQPeak::average_kbps`); `Reset()` restores both.
- VA-API: `QVBR` where the driver lists it (`vaapi_device_rc_supported`), with
  `maxrate` as the peak; check what the driver does with `bitrate` and whether
  the encoder advertises `OBS_ENCODER_CAP_DYN_BITRATE` (it does not today).

Whatever measures as not holding stays CBR. Decided by the maintainer: "Use
OBS Output settings" in Simple mode stays CBR, and every encoder keeps its own
default quality level (x264 CRF 23, NVENC target quality 20) until the VMAF
harness ([quest/m9/video-quality](/quest/m9/video-quality/README.md)) picks one. Extend `EnforceCap` and
`EncoderPeakKey` for the mode, and the retune and restore in `moq-output.cpp` for
the setting a grant writes.

Public API: none. Wire: none.

## Hosts

- macos - VideoToolbox's rate limit can only be measured on a Mac
- linux - VA-API's quality mode depends on the driver, so it is measured on one
