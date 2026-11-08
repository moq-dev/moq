# [S] VA-API encode is correct on radeonsi

## Goal

On an AMD RX 9070 (radeonsi), moq-video's VA-API H.264 encoder produces a
stream that ffmpeg decodes, its VPP NV12 conversion matches the space it
labels, and its rate control stays near the target bitrate. The VA-API
tests in `rs/moq-video/src/encode/backend/vaapi.rs`, ignored ones included,
pass on that card with `MOQ_VAAPI_DEVICE` naming its node.

## Plan

The causes were in moq-vaapi and are fixed in
[moq-dev/vaapi#13](https://github.com/moq-dev/vaapi/pull/13). With it patched
in, all 19 of moq-video's VA-API tests pass on the RX 9070 (`renderD129`,
Mesa 26.2.5); on moq-vaapi 0.2.0, 7 fail. That PR's body has the evidence.

- H.264 bytes: radeonsi writes the packed SPS and PPS, and the slice's NAL
  header, only from a packed slice header, which moq-vaapi did not send.
- Bitrate: moq-vaapi's QP ceiling of 36 set a smallest picture on both iHD
  and radeonsi; it is now 51.
- Chroma: not an offset. Mesa's VPP reads the BT.601 standard as SMPTE 170M
  primaries and gamut-maps; moq-vaapi now names each side explicitly on Mesa.

Decided 2026-11-07 (maintainer, /quest-unblock): an agent merged #13 and
published it as moq-vaapi 0.2.1 (a patch: `cargo semver-checks` against 0.2.0
needs no bump), tagged `v0.2.1`.

What remains here: bump `moq-vaapi` in the root `Cargo.toml` to 0.2.1, refresh `Cargo.lock` with it, and say in the module docs of
`rs/moq-video/src/encode/backend/vaapi.rs` that the tests pass on radeonsi.
Then delete this quest.

Public API: none. Wire: none.

## Related

- [Vulkan Video encode on AMD](/quest/m3/vulkan-encode.md) - AMD's long-term encode path
- [VA-API renderer tests](/quest/m4/vaapi-render-tests.md) - runs the VA-API tests on each render node
- [moq-vaapi vendor sync](/quest/m3/vendor-sync/vaapi.md) - the crate's upstream record
