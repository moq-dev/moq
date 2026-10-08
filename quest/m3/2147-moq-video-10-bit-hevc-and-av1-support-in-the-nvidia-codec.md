# [M] Complete NVIDIA Main10 encoding support

## Goal

Implement and verify the remaining 10-bit HEVC encoding work tracked in
[#2147](https://github.com/moq-dev/moq/issues/2147). NVDEC AV1 decoding and
catalog AV1 types already exist; do not reimplement them.

## Plan

Decided in the 2026-09-30 audit: AV1 encoding split out to
[NVENC AV1](/quest/m3/nvenc-av1.md), because it needs an Ada GPU and the only
GPU CI host is an RTX 3070 Ti. Main10 stays here since that host can verify it.

Decided 2026-10-08: moved to m3. No 10-bit encode pipeline or consumer
exists yet.

Extend the settled frame and NVENC contracts with Main10 surfaces, profile
selection, and accurate codec metadata. Audit byte pitch, plane layout, CPU
download, and P016 input/output together; a codec enum alone does not establish
10-bit support. Preserve color metadata and refuse unsupported conversions.
Do not imply that OpenH264 can decode HEVC or tonemap HDR.

Use existing extensible codec enums; no replacement of the 0.1 core API is
planned. Keep the NVIDIA backend optional and loaded at runtime.

Validate decoded pixels, bit depth, profile, resource lifetime, drain, and
refusal on unsupported devices. Wire fixtures and contract tests into CI, and
run the hardware tests on the [GPU CI](/quest/m1/gpu-ci.md) host.

Public API: additive capabilities on the extension points settled on main. Wire: existing
codec signaling, with cross-language fixtures for any metadata change.

## Closes

- [#2147](https://github.com/moq-dev/moq/issues/2147) - close this issue when the quest finishes

## Related

- [GPU CI](/quest/m1/gpu-ci.md) - the RTX 3070 Ti host that verifies Main10
- [NVENC AV1](/quest/m3/nvenc-av1.md) - the AV1 half of #2147, hardware-gated
- [Codec coverage study](/quest/m3/video-codec-coverage.md) - measure optional software and other native backends separately
