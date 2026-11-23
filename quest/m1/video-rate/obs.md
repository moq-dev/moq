# [S] OBS runs capped VBR on VideoToolbox

## Goal

The OBS plugin's dock profiles run VideoToolbox (macOS) at constant quality
under the profile's peak, on an OBS whose VideoToolbox encoder passes that peak
to macOS, instead of the CBR it runs today.

## Plan

The epic's decisions apply: quality mode with a peak where honored, otherwise
CBR.

`EnforceCap` in `cpp/obs/src/moq-encoder-rate.h` owns the cap on every video
encoder the MoQ output starts. x264 and NVENC run constant quality under it;
Quick Sync, AMF, and VA-API stay CBR for good
([moq-dev/moq#9193](https://github.com/moq-dev/moq/pull/9193) measured VA-API).
VideoToolbox stays CBR until OBS passes its limit: through OBS 32.2.2 the
limit reaches macOS empty, so CRF runs with no peak
([moq-dev/moq#9473](https://github.com/moq-dev/moq/pull/9473)).

Measured on an Apple M4 (macOS 26.6) by replaying OBS's session setup with the
limit's numbers created correctly: CRF at OBS's default quality 60 with a
3 Mbps limit over 1 s holds the cap better than CBR does. Worst 1 s windows, in
kbps, against a 3 Mbps cap:

| Clip (40 s, 720p30) | H.264 CBR | H.264 CRF + limit | HEVC CBR | HEVC CRF + limit |
|---|---|---|---|---|
| Big Buck Bunny | 4556 | 2790 | 4547 | 2719 |
| Heavy noise | 4052 | 2622 | 4552 | 2495 |
| Light grain | 3771 | 2589 | 3868 | 2570 |
| Still frame | 3561 | 1165 | 3555 | 955 |
| Still, then noise | 10602 | 7728 | 11717 | 6466 |

It averages 0.4 to 1.5 Mbps on calm clips, where CBR spends 3 Mbps, but only
2.0 Mbps on noise and grain at every window from 0.5 s to 2 s: VideoToolbox
settles at two thirds of the limit, so hard scenes get fewer bits than CBR
gives them. Taking capped VBR anyway follows the epic's decision; the VMAF
harness ([quest/m9/video-quality](/quest/m9/video-quality/README.md)) can tell
whether the hard-scene loss shows. A 0.5 s window also held the worst 0.5 s
window under the cap on noise (2939 kbps), where 1 s let it reach 3190.

Remaining, once a fixed OBS ships:

- Decide at setup from `obs_get_version()`: an OBS at or past the fixed release
  runs `rate_control` `CRF` with `limit_bitrate`, `max_bitrate` at the cap, and
  `max_bitrate_window`; an older one stays CBR. `EncoderPeakKey` reports
  `max_bitrate` for it. Also write the limit beside CBR: Apple Silicon ignores
  it there, but an Intel Mac, where OBS runs both CBR and CRF as ABR, then holds
  the peak instead of only an average.
- `vt_update` re-applies the rate only on a `bitrate` change, so `SetBitrate`
  writes `bitrate` beside `max_bitrate`, clamped to the granted peak, as it does
  for NVENC's plain VBR (`MoQPeak::average_kbps`); `Reset()` restores both.
- Repeat the measurement through the fixed OBS itself.

Decided by the maintainer: "Use OBS Output settings" in Simple mode stays CBR,
and every encoder keeps its own default quality level (x264 CRF 23, NVENC
target quality 20; VideoToolbox's is 60) until the VMAF harness picks one.

Public API: none. Wire: none.

## Hosts

- macos - VideoToolbox's rate limit can only be measured on a Mac

## Required

- [OBS passes VideoToolbox its bitrate limit](/quest/m1/video-rate/obs-vt-limit.md) - an OBS release whose `max_bitrate` reaches the encoder
