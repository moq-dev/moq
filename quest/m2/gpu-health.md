# [M] Vendor-neutral GPU capacity and health

## Goal

moq-video reports, per GPU device, what an admission controller needs to decide
whether one more encode or decode fits: encoder and decoder sessions in use
and any hard session limit, memory used and available, and engine
utilization. It also reports device health (whether the device is usable,
lost, or reset) and the GPU model, which moq.pro's admission keys its
per-model limits on. The same API works on NVIDIA, AMD, and Intel, keyed by
the device identity `Surface::Vulkan` carries (`frame::vulkan::Device`), so a
caller writes no vendor code. A value a driver cannot provide is absent, never guessed.

Not here: admission policy, thresholds, frame-lateness tracking, or draining.
Those are the caller's.

## Plan

Decided 2026-10-05 in moq.pro's quest audit: vendor-neutral now, not
NVIDIA-scoped. Reason: the external GPU surface (#4975),
[Vulkan encode](/quest/m2/vulkan-encode.md), and
[VA-API import](/quest/m2/vaapi-vulkan-import.md) make AMD and Intel encode
first-class, so an NVML-shaped API would be rewritten once they land.
moq.pro's transcode admission and its benchmarks assume NVML today; they
adopt this instead.

Decided 2026-10-05: one device identity wherever a render node exists, the
render node's `dev_t`, shared with [VA-API import](/quest/m2/vaapi-vulkan-import.md).
NVIDIA's device UUID maps to its node through `VK_EXT_physical_device_drm`.
The external GPU surface (#4975) carries that identity as
`frame::vulkan::Device { device_uuid, driver_uuid, render_node: Option<u64> }`.
`render_node` is absent when the exporter has no DRM render node; key such a
device by its `device_uuid`, which every Vulkan device has, so a report never
merges two devices or drops one.

Guidance, to be settled while building:

- Prefer one source that spans vendors over three vendor paths. Candidates to
  evaluate: `VK_EXT_memory_budget` for memory, DRM fdinfo or sysfs engine
  busyness for utilization on amdgpu, i915, and xe, and on NVIDIA,
  `NV_ENC_CAPS_DYNAMIC_QUERY_ENCODER_CAPACITY` through moq-nvenc's
  `get_encode_caps`, with NVML only where NVIDIA offers nothing neutral.
  NVML may not report the consumer-card encoder session cap at all; verify
  that while building rather than assume it.
  Record which source backs each field per vendor, and whether each memory
  value is process-scoped or device-wide: `VK_EXT_memory_budget`'s
  `heapUsage` is this process's only, so device-wide usage needs a driver
  source (sysfs or NVML). DRM fdinfo is per client: summing it covers only
  the clients this process can see and may double-count shared buffers, so
  report it as client-scoped unless building shows a sound device-wide sum.
- Sessions moq-video opened itself are counted in process; device-wide counts
  come from the driver where it reports them, since several processes may
  share a GPU.
- Health covers a lost or reset device, surfaced from the backends' own
  errors (`VK_ERROR_DEVICE_LOST`, CUDA and VA-API equivalents) and from the
  driver's reset counters where exposed.
- A snapshot call, not a callback; cheap enough to poll about once a second.
- The snapshot is additive, but it keys by the surface's identity, and the
  surface change is breaking, so it reaches `release` with
  [the multi-vendor release](/quest/m2/gpu-release.md), not by an earlier
  backport.

Test: a unit test of each source's parsing against recorded fixtures, and
ignored hardware tests in `just rs gpu`'s per-vendor branches (the recipe
#4975 added) that open an encoder and see
the session and memory counts move.

Public API: a device capacity and health snapshot in moq-video. Wire: none.

## Related

- [VA-API encodes an external Vulkan image](/quest/m2/vaapi-vulkan-import.md) - picks a render node by the same `dev_t`
- [Vulkan Video encode on AMD](/quest/m2/vulkan-encode.md) - the AMD backend whose sessions this counts
- [moq.pro: transcode admission](https://github.com/moq-dev/moq.pro/blob/main/quest/m2/transcode/ws3-admission.md) - the admission and health checks that consume this
