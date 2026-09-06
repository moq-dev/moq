# [XL] Present native moq-video decoded frames in OBS on Linux

## Goal

At least one supported Linux hardware decoder delivers frames to OBS without CPU readback. Other machines keep working through the explicit CPU delivery fallback.

## Plan

- Probe decoder output and OBS EGL/GL import compatibility before promising broad support. This checkout has NVDEC decoding but no VAAPI decoder; existing VAAPI work owns adding that backend. Choose an available native path based on actual hardware evidence and report which drivers/devices were tested.
- For DMA-BUF, validate device, modifiers, planes, stride, color conversion, fd ownership, and implicit/explicit synchronization. For CUDA output, establish a real graphics interop path or GPU copy into importable storage. A DMA-BUF type or OBS import function alone does not prove the decoder allocation can be presented.
- Bound imported allocations and retain them through GPU completion. Handle device loss, resize, stale frames, and failed imports with automatic CPU fallback shown in Stats. Never use an FFmpeg fallback.
- Verify pixels, timestamps, latency, no CPU readback, cancellation, resource growth, and fallback on real Linux graphics stacks. Keep VAAPI and NVIDIA validation results separate; one driver passing is not platform-wide proof.

## Required

- [Video source replacement](/quest/m2/obs-moq-video/source.md) - native frame contract and fallback lifecycle

## Related

- [VAAPI encode and decode](/quest/m2/video-vaapi.md) - owns VAAPI decoder and native surface support; avoid a duplicate backend implementation
