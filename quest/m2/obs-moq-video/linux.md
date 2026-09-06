# [XL] Export OBS GPU frames for moq-video on Linux

## Goal

A supported Linux OBS graphics/encoder combination publishes composited video without CPU readback and reports unsupported combinations explicitly.

## Plan

- Start with an API feasibility probe. OBS exposes DMA-BUF import in `graphics.h`; that is not proof its compositor textures can be exported. Determine whether EGL/GL allocation export is available, or whether an upstream OBS hook or an encoder-owned exportable render target is required.
- Negotiate DRM device, fourcc, plane offsets/strides, modifiers, and synchronization. Reuse `Surface::DmaBuf` and the hardware encoder's real import path, retaining allocation ownership until completion. A borrowed fd or an importable packed RGB texture does not prove the encoder accepts NV12 on the same device.
- Prefer direct import; otherwise GPU-convert into exportable NV12 surfaces. Document and measure each GPU copy. Keep CPU staging as a visible fallback, not as the successful accelerated result.
- Validate at least one Intel/AMD VAAPI path on hardware before promising broad support. Scope NVIDIA/CUDA interoperability separately if it needs another allocation or synchronization strategy. Test unsupported modifiers, multi-plane buffers, fd closure, cancellation, device loss, resize, and repeated pool reuse.
- Capture API traces/copy counts and decoded pixel/timestamp checks with a real subscriber. Compare latency percentiles, utilization, and resource growth against the CPU adapter and existing OBS encoder at matched settings.

## Required

- [Encoder adapter](/quest/m2/obs-moq-video/adapter.md) - frame ownership, queue policy, packet output, and comparison baseline

## Related

- [Video hardware validation](/quest/m3/video-hardware.md) - native input and encoder acceptance need hardware evidence
