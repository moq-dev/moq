# [L] Complete NVIDIA Main10 and AV1 encoding support

## Goal

Implement and verify the remaining 10-bit HEVC and AV1 encoding work tracked
in [#2147](https://github.com/moq-dev/moq/issues/2147). NVDEC AV1 decoding and
catalog AV1 types already exist; do not reimplement them.

## Plan

Extend the settled frame and NVENC contracts with Main10 surfaces, profile
selection, and accurate codec metadata. Audit byte pitch, plane layout, CPU
download, and P016 input/output together; a codec enum alone does not establish
10-bit support. Preserve color metadata and refuse unsupported conversions.
Do not imply that OpenH264 can decode HEVC or tonemap HDR.

Add AV1 encoding only where the queried NVIDIA device/driver supports it.
Preserve OBU framing and accurate catalog configuration through transcode.
Use existing extensible codec enums; no replacement of the 0.1 core API is
planned. Keep the NVIDIA backend optional and loaded at runtime.

Split Main10 and AV1 implementation into independent PRs if hardware or review
scope warrants it. Validate decoded pixels, bit depth, profile, framing,
resource lifetime, drain, and refusal on unsupported devices. Wire fixtures
and contract tests into CI, and record actual hardware execution separately.
Lack of suitable hardware leaves that implementation unverified, not complete.

Public API: additive capabilities on the extension points settled on main. Wire: existing
codec signaling, with cross-language fixtures for any metadata change.

## Closes

- [#2147](https://github.com/moq-dev/moq/issues/2147) - close this issue when the quest finishes

## Related

- [Codec coverage study](/quest/m2/video-codec-coverage.md) - measure optional software and other native backends separately
