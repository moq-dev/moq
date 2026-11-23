# [XS] OBS passes VideoToolbox its bitrate limit

## Goal

An OBS release whose VideoToolbox encoder hands macOS the limit its **Limit
bitrate** setting (`limit_bitrate`, `max_bitrate`, `max_bitrate_window`) names,
so the plugin can run VideoToolbox's CRF under the cap.

## Plan

In OBS 32.2.2, and on obs-studio's `master` as of 2026-11-23,
`session_set_bitrate` in `plugins/mac-videotoolbox/encoder.c` builds the
`kVTCompressionPropertyKey_DataRateLimits` pair from two `double`s but creates
the numbers as `kCFNumberIntType` and `kCFNumberFloatType`, so CoreFoundation
reads the low 32 bits of each. Those bits are zero for ordinary values (3000
kbps over 1 or 1.5 s), so macOS accepts `[0, 0]` and keeps no limit at all. On
an Apple M4, OBS's CRF with the limit came out byte-identical to CRF without
it, up to 59 Mbps against a 3 Mbps cap; created as `kCFNumberLongLongType` bytes
and `kCFNumberDoubleType` seconds, the limit is stored and held
([moq-dev/moq#9473](https://github.com/moq-dev/moq/pull/9473)).

## Waiting

- Maintainer: report the bug to obsproject/obs-studio, or send the fix. Their
  contributing guide requires issues, comments, and code to be human written,
  so no agent may file or draft it. Done when the upstream issue or PR is
  linked here.
- obsproject/obs-studio: ship the fix in a release. Check by reading
  `session_set_bitrate` in `plugins/mac-videotoolbox/encoder.c` at the newest
  release tag.
