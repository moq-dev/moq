# [M] VA-API encode is correct on radeonsi

## Goal

On an AMD RX 9070 (radeonsi), moq-video's VA-API H.264 encoder either
produces a stream that ffmpeg decodes, or refuses the node, so `Kind::Auto`
falls to the next H.264 encoder (OpenH264 in a build that has it; a build
without one gets `NoEncoder` naming the refused driver). Its VPP NV12 conversion carries no chroma offset, and its
rate control stays near the target bitrate. The ignored VA-API encode tests
in `rs/moq-video/src/encode/backend/vaapi.rs` pass on that card with
`MOQ_VAAPI_DEVICE` naming its node, or skip with a reason that names the
refused driver.

## Plan

Found 2026-11-06 in [moq-dev/moq#8013](https://github.com/moq-dev/moq/pull/8013)'s
hardware run on an RX 9070 (Mesa 26.0.8, `renderD129`): 6 of 19 VA-API tests
failed, all of them H.264 encode. A 320x240 dump gives ffprobe
`Invalid data found when processing input`, and its first NAL unit has type
0. This also reproduces on main's moq-vaapi. H.265 encode and both decoders
pass there. [moq-dev/vaapi#10](https://github.com/moq-dev/vaapi/pull/10)
also noted a chroma offset in radeonsi's VPP NV12 output and a bitrate
overshoot.

Decided 2026-11-06 (maintainer, /quest-unblock): ✅ keep #8013 queued, which
puts `vaapi` on by default in moq-cli and moq-transcode, and fix this in m1.
Until it lands, an AMD-only host encodes H.264 through VA-API and publishes
a stream nobody can decode; moq-c and moq-ffi already did. Not chosen:
pulling #8013 to refuse radeonsi first, or keeping `vaapi` opt-in. Also
decided: ✅ find the root cause in m1. Not chosen: m3 beside
[Vulkan Video encode on AMD](/quest/m3/vulkan-encode.md), or only a note
there.

- Find whether the H.264 bytes are ours. NAL type 0 at the start points at
  how moq-vaapi reads the coded buffer (a segment's `bit_offset`, or more
  than one segment) or at packed headers radeonsi does not take for H.264.
  [moq-dev/vaapi#10](https://github.com/moq-dev/vaapi/pull/10) had to
  require packed H.265 parameter sets for the same reason. Compare against
  `ffmpeg -c:v h264_vaapi` on the same node.
- If it is moq-vaapi's, fix it there with a test, release the crate, and bump
  `moq-vaapi` here. If it is the driver's, refuse a radeonsi node for H.264
  encode, using the driver's vendor string, so the candidate fails to open and
  OpenH264 takes over. Then link the Mesa issue.
- Treat the chroma offset and the bitrate overshoot the same way: fix them if
  they are ours, and if they are the driver's, record them with their Mesa
  issue.
- Vulkan Video still replaces VA-API on AMD later; this quest does not wait
  on it.

Public API: none. Wire: none.

## Related

- [Vulkan Video encode on AMD](/quest/m3/vulkan-encode.md) - AMD's long-term encode path
- [VA-API renderer tests](/quest/m4/vaapi-render-tests.md) - runs the VA-API tests on each render node
- [moq-vaapi vendor sync](/quest/m3/vendor-sync/vaapi.md) - the crate's upstream record
