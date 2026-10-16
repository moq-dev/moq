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
[Vulkan encode](/quest/m1/vulkan-encode.md), and
[VA-API import](/quest/m1/vaapi-vulkan-import.md) make AMD and Intel encode
first-class, so an NVML-shaped API would be rewritten once they land.
moq.pro's transcode admission and its benchmarks assume NVML today; they
adopt this instead.

Decided 2026-10-05: one device identity wherever a render node exists, the
render node's `dev_t`, shared with [VA-API import](/quest/m1/vaapi-vulkan-import.md).
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

### Source audit (2026-10-16)

There is no single Linux interface that supplies the promised device-wide
snapshot across NVIDIA, AMD, and Intel:

- Vulkan
  [`VK_EXT_memory_budget`](https://registry.khronos.org/vulkan/specs/latest/man/html/VK_EXT_memory_budget.html)
  reports the current process's estimated heap usage and allocation budget,
  not device-wide used and total memory. It cannot back the admission fields.
- DRM
  [`drm-memory-*`](https://docs.kernel.org/gpu/drm-usage-stats.html) and engine
  counters in fdinfo are scoped to one DRM client. They are useful for
  attributing this process, but cannot provide device-wide sessions, memory,
  utilization, or health.
- AMDGPU exposes device-wide
  [`mem_info_vram_total` and `mem_info_vram_used`](https://docs.kernel.org/gpu/amdgpu/driver-misc.html),
  but those sysfs files are driver-specific and do not supply codec sessions
  or a portable reset state.
- NVIDIA's
  [NVML device queries](https://docs.nvidia.com/deploy/nvml-api/latest/api/group__nvmlDeviceQueries.html)
  supply device memory, encoder sessions and utilization, decoder utilization,
  model, and lost/reset-required errors. That is the only evaluated source
  that covers most of the snapshot, and it is vendor-specific.

The implementation therefore needs vendor adapters behind one public schema;
"vendor-neutral" describes the caller API, not its data source. Keep every
field optional and attach a scope (`Process` or `Device`) so partial adapters
cannot accidentally present process-local data as admission capacity.

Before implementation, decide the public shape and the first honest support
floor:

1. **Recommended:** `gpu::snapshot(&frame::vulkan::Device)` returns one
   additive `gpu::Snapshot` with scoped optional measurements. Land NVIDIA's
   device-wide adapter and the in-process encoder/decoder counts first; add
   AMD and Intel adapters when their hardware quests can prove equivalent
   fields. Unsupported fields remain absent.
2. Require the whole promised field set on all three vendors before exposing
   the API. This keeps a stronger contract, but blocks the quest on hardware
   access and on driver interfaces that do not currently expose equivalent
   device-wide codec-session data.
3. Expose a provider trait for callers to supply vendor data. This avoids an
   in-tree NVML adapter, but leaks backend selection and lifecycle into every
   admission caller.

## Related

- [VA-API encodes an external Vulkan image](/quest/m1/vaapi-vulkan-import.md) - picks a render node by the same `dev_t`
- [Vulkan Video encode on AMD](/quest/m1/vulkan-encode.md) - the AMD backend whose sessions this counts
