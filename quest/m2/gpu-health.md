# [M] Vendor-neutral GPU capacity and health

## Goal

moq-video reports, per GPU device, what an admission controller needs to decide
whether one more encode or decode fits: encoder and decoder sessions in use
and any hard session limit, memory used and available, and engine
utilization. It also reports device health: whether the device is usable,
lost, or reset. The same API works on NVIDIA, AMD, and Intel, keyed by the
device identity [`Surface::Vulkan`](/quest/m2/gpu-surface.md) matches on, so a
caller writes no vendor code. A value a driver cannot provide is absent, never
guessed.

Not here: admission policy, thresholds, frame-lateness tracking, or draining.
Those are the caller's.

## Plan

Decided 2026-10-05 in moq.pro's quest audit: vendor-neutral now, not
NVIDIA-scoped. Reason: [GPU surface](/quest/m2/gpu-surface.md),
[Vulkan encode](/quest/m2/vulkan-encode.md), and
[VA-API import](/quest/m2/vaapi-vulkan-import.md) make AMD and Intel encode
first-class, so an NVML-shaped API would be rewritten once they land.
moq.pro's transcode admission and its benchmarks assume NVML today; they
adopt this instead.

Guidance, to be settled while building:

- Prefer one source that spans vendors over three vendor paths. Candidates to
  evaluate: `VK_EXT_memory_budget` for memory, DRM fdinfo or sysfs engine
  busyness for utilization on amdgpu, i915, and xe, and NVML only where
  NVIDIA offers nothing neutral (encoder session limits on consumer cards).
  Record which source backs each field per vendor.
- Sessions moq-video opened itself are counted in process; device-wide counts
  come from the driver where it reports them, since several processes may
  share a GPU.
- Health covers a lost or reset device, surfaced from the backends' own
  errors (`VK_ERROR_DEVICE_LOST`, CUDA and VA-API equivalents) and from the
  driver's reset counters where exposed.
- A snapshot call, not a callback; cheap enough to poll about once a second.
- Additive, so it can reach `release` by backport without waiting for
  [the multi-vendor release](/quest/m2/gpu-release.md).

Test: a unit test of each source's parsing against recorded fixtures, and
ignored hardware tests in `just rs gpu`'s per-vendor branches that open an
encoder and see the session and memory counts move.

Public API: a device capacity and health snapshot in moq-video. Wire: none.

## Related

- [One external GPU image for every encoder](/quest/m2/gpu-surface.md) - the device identity both key by
- [Vulkan Video encode on AMD](/quest/m2/vulkan-encode.md) - the AMD backend whose sessions this counts
- [moq.pro: transcode admission](https://github.com/moq-dev/moq.pro/blob/main/quest/m2/transcode/ws3-admission.md) - the admission and health checks that consume this
