# [L] Capped VBR on Linux encoders

## Goal

NVENC, VA-API, and openh264 in `moq-video` encode constant quality under a
hard peak at the rendition's cap, and `set_bitrate` moves that peak. V4L2 and
Android MediaCodec, which have no quality mode, stay CBR at the cap. The PR
measures how far each backend's average falls below the cap on static, talking
head, and high-motion clips, and how long a native publisher takes to reach
the cap after a quiet stretch.

## Plan

The epic's decisions apply: quality mode with a peak where the backend honors
one, decided at setup from its capabilities, otherwise CBR at the cap; bitrate
`min(cap, grant)`; no public quality knob.

Today, from `rs/moq-video/src/encode/backend/`:

- NVENC runs `NV_ENC_PARAMS_RC_CBR` with a one-frame VBV. Candidate:
  `NV_ENC_PARAMS_RC_VBR` with `targetQuality` and `maxBitRate` at the cap,
  keeping a short VBV so a peak stays a peak.
- VA-API runs `VA_RC_CBR` with frame skip disabled. Candidate: `VA_RC_QVBR`
  (or ICQ with a max) where `VAConfigAttribRateControl` advertises it, with the
  HRD buffer sent; CBR elsewhere, chosen at setup.
- openh264 runs `RC_BITRATE_MODE` with the crate's frame skip on. Candidate:
  `RC_QUALITY_MODE` with `ENCODER_OPTION_MAX_BITRATE`.
- V4L2 has `V4L2_CID_MPEG_VIDEO_BITRATE_PEAK` but no quality mode, and
  MediaCodec's VBR has no peak: both keep CBR.

Pick each backend's quality level by measurement, aiming for 720p30 talking
head that looks transparent, and record the numbers. A backend whose quality
mode overshoots the cap in measurement uses CBR instead.

Validation, beyond the per-backend averages: on a bufferbloat-shaped link,
hold a static scene for 30 s, then switch to high motion, and report the time
until the encoder runs at the cap and the latency on the way. A slow climb is
a finding for [probing](/quest/m3/probe/README.md), not a reason to override
the estimate (decided 2026-11-15).

Public API: none. Wire: none.

## Hosts

- linux - NVENC and VA-API need the GPUs on `fedora` and `desktop`

## Related

- [Probe up to a target bitrate](/quest/m3/probe/README.md) - recovers the estimate after a quiet stretch
- [Video quality](/quest/m9/video-quality/README.md) - its harness can score the chosen quality levels
