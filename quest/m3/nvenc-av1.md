# [M] NVENC AV1 encoding

## Goal

The NVENC backend encodes AV1 where the queried NVIDIA device and driver
support it, with OBU framing and an accurate catalog configuration through
transcode, and refuses it elsewhere.

## Plan

Split from the Main10 quest in the 2026-09-30 audit. It sits in m3 because it
is hardware-gated: AV1 NVENC needs an Ada GPU, and the only GPU CI host is an
RTX 3070 Ti. An Ada GPU on a CI host, or a consumer asking for AV1 encode,
brings it back.

Query the capability at construction and refuse without it. Use existing
extensible codec enums; keep the NVIDIA backend optional and loaded at
runtime. Validate decoded pixels, profile, framing, drain, and refusal on
unsupported devices. Lack of suitable hardware leaves this unverified, not
complete.

Public API: additive. Wire: existing AV1 codec signaling.

## Required

- [An Ada NVIDIA GPU is available](/quest/m3/ada-gpu.md) - the hardware to verify on

## Related

- [Main10](/quest/m2/2147-moq-video-10-bit-hevc-and-av1-support-in-the-nvidia-codec.md) - the other half of #2147
- [GPU CI](/quest/m1/gpu-ci.md) - the current runner lacks AV1 NVENC
