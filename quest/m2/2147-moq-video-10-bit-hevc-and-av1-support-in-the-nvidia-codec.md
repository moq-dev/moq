# [M] moq-video: 10-bit HEVC and AV1 support in the NVIDIA codec path

## Goal

Implement and verify the behavior tracked in [#2147](https://github.com/moq-dev/moq/issues/2147)
within the issue's stated scope and boundaries.

## Plan

Rescoped during the 2026-08 grooming: AV1 8-bit decode landed on dev (#2178).
Remaining: 10-bit Main10 decode and encode (P016 surfaces, profile plumbing),
and AV1 encode/transcode once Ada+ hardware is available.

### Issue context

Follow-up to #2145 (NVDEC hardware decode + zero-copy NVDEC -> NVENC transcode). The GPU pipeline currently supports 8-bit 4:2:0 H.264/H.265 only. Two extensions worth doing, probably as separate PRs:

#### 10-bit HEVC (Main10)

- The NVDEC backend rejects `bit_depth_luma_minus8 != 0` today (clean error in the sequence callback). Supporting it means decoding to P016 output surfaces and threading a pixel-format dimension through `frame::cuda::Frame` (pitch is in bytes, but plane layout and the CPU download path assume 8-bit NV12).
- NVENC needs the matching Main10 profile + `NV_ENC_BUFFER_FORMAT_YUV420_10BIT` input, and the catalog codec string must advertise the right profile/level.
- The CPU fallback story needs deciding: openh264 is 8-bit only, so a 10-bit source either has no software fallback (like H.265 already) or gets tonemapped down to 8-bit.

#### AV1

- Decode: NVDEC supports AV1 on Ampere+ (`cudaVideoCodec_AV1` is already in the vendored bindings). Needs a `Codec::Av1` in the decode backend seam, the catalog/container plumbing for AV1 tracks, and AV1 has no Annex-B: the parser takes OBUs directly, so the access-unit prep differs from H.264/H.265.
- Encode: NVENC AV1 exists only on Ada+ (the RTX 3070 Ti dev box can decode AV1 but not encode it), so the first useful shape is AV1 *source* -> H.264/H.265 rungs in `moq-transcode`, not AV1 output.
- The `hang` catalog and `moq-mux` need an AV1 codec entry (`av01.*` codec string, OBU framing) if they don't have one by then; that part rows through the js side per the cross-package sync table.

Both are additive to the decode/encode `Codec` enums (`#[non_exhaustive]` already), so no breaking changes expected.

AV1 encode belongs here too: it was waiting on a hardware backend, and the
NVIDIA codec path is that backend. No software AV1 encode, since rav1e is too
slow for real time.

## Closes

- [#2147](https://github.com/moq-dev/moq/issues/2147) - close this issue when the quest finishes
