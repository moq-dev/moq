# [S] H.265 rungs respect the encoder's minimum size

## Goal

A moq-transcode ladder whose H.265 rung is narrower than the hardware
encoder's minimum never fails that rendition. The encoder probe reports the
driver's minimum width and height, and the ladder widens or drops the rung
below it instead of failing to open an encoder.

## Plan

Found 2026-11-06 in [moq-dev/moq#8013](https://github.com/moq-dev/moq/pull/8013).
AMD's VCN 5 (radeonsi, Mesa `si_get.c`) does not encode H.265 narrower than
384. moq-video has no software H.265 encoder, so on an AMD-only host a
rendition below that fails to open. The default ladder's 240p rung is 320
wide for a 4:3 source.

Decided 2026-11-06 (maintainer, /quest-unblock): ✅ an S quest in m3. Not
chosen: folding it into [the radeonsi quest](/quest/m1/vaapi-radeonsi.md),
or dropping it.

- Read the minimum from the driver rather than hard-coding 384, such as
  through VA-API's `VAConfigAttribMaxPictureWidth` and minimum picture
  attributes, or the encoder's open error. Check what NVENC and VideoToolbox
  report for the same case.
- Choose between widening the rung to the minimum and dropping it, and
  record why. A dropped rung must stay out of the catalog.

Public API: maybe a minimum size on the encoder probe in moq-video. Wire:
none.

## Related

- [VA-API encode is correct on radeonsi](/quest/m1/vaapi-radeonsi.md) - the same AMD hardware run
- [Vulkan Video encode on AMD](/quest/m3/vulkan-encode.md) - may report a different minimum
