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

Source audit, 2026-10-16: no single Linux interface spans the three vendors,
so "vendor-neutral" describes the caller's API, not its data source. Each
field comes from the narrowest source that reports it, and is absent where
none does:

| Field | NVIDIA | AMD (amdgpu) | Intel (i915, xe) |
|---|---|---|---|
| Sessions moq-video opened | counted in process | counted in process | counted in process |
| Sessions device-wide | NVML encoder sessions; decode absent | absent | absent |
| Hard session limit | absent unless NVML proves the consumer cap | absent, none known | absent, none known |
| Memory headroom for this process | `VK_EXT_memory_budget` | `VK_EXT_memory_budget` | `VK_EXT_memory_budget` |
| Memory used and total, device-wide | NVML | sysfs `mem_info_vram_*` | absent until a source is proven |
| Video engine utilization | NVML encoder and decoder utilization | `gpu_metrics` VCN activity, where the ASIC reports it | PMU video engine busyness: i915 `vcs*-busy`, xe `engine-active-ticks` |
| Health | NVML errors, `VK_ERROR_DEVICE_LOST` | DRM wedged uevent, `VK_ERROR_DEVICE_LOST` | DRM wedged uevent, i915 `RESET` uevent, `VK_ERROR_DEVICE_LOST` |
| Model | Vulkan `deviceName` | Vulkan `deviceName` | Vulkan `deviceName` |

Sources:
[NVML device queries](https://docs.nvidia.com/deploy/nvml-api/latest/api/group__nvmlDeviceQueries.html),
[`VK_EXT_memory_budget`](https://docs.vulkan.org/refpages/latest/refpages/source/VK_EXT_memory_budget.html),
[amdgpu sysfs](https://docs.kernel.org/gpu/amdgpu/driver-misc.html),
[amdgpu `gpu_metrics`](https://docs.kernel.org/gpu/amdgpu/thermal.html),
[DRM device wedging](https://docs.kernel.org/gpu/drm-uapi.html#device-wedging),
[DRM fdinfo](https://docs.kernel.org/gpu/drm-usage-stats.html).

Notes on scope:

- `VK_EXT_memory_budget`'s `heapUsage` is this process's allocation, and
  `heapBudget` is the driver's estimate of what this process may use given
  other activity on the system. Budget minus usage is the one memory figure
  every vendor provides, but it is headroom for this process, not device-wide
  use.
- DRM fdinfo counters are per client. They attribute this process's use but
  cannot sum to a device-wide figure, so they back no field above.
- Intel's PMU engine counters are device-wide per engine but need perf access
  (`CAP_PERFMON` or a relaxed `perf_event_paranoid`). xe's engine activity
  events are recent (GuC-backed) and listed under
  `/sys/bus/event_source/devices/xe_*/events`; where the event or the access
  is missing, the field is absent. i915's busy counter is nanoseconds, so
  utilization is its delta over the elapsed time between reads. xe's
  `engine-active-ticks` must be read in one perf group with
  `engine-total-ticks`, and utilization is the ratio of their deltas.
- amdgpu, i915, and xe send the DRM wedged uevent when a device is unusable;
  its `WEDGED=` method names the recovery it needs. A reset the driver
  recovered from (usable, but its work was lost) arrives differently per
  driver: amdgpu sends the wedged uevent with `WEDGED=none` after a
  successful ring reset, i915 sends its own `RESET=1` then `ERROR=0`
  uevents, and xe sends nothing, so a recovered reset caused by another
  process is absent on xe. Uevents are one-shot, so seeing them needs a
  listener that outlives a single poll.
- NVML may not report the consumer-card encoder session cap at all; verify on
  hardware rather than assume it.

Other guidance:

- Sessions moq-video opened itself are counted in process; device-wide counts
  come from the driver where it reports them, since several processes may
  share a GPU. A value's scope (this process or the whole device) is part of
  its name, so process-local data never reads as device capacity.
- Health also comes from the backends' own errors: `VK_ERROR_DEVICE_LOST` and
  its CUDA and VA-API equivalents.
- A snapshot call, not a callback; cheap enough to poll about once a second.
- The snapshot is additive and keys by the surface's identity. Publish it
  after the vendor implementations it reports are validated. It does not gate
  [the multi-vendor release](/quest/m2/gpu-release.md) or the next cut.

Open, support floor (recommended first):

1. Ship the table as it stands: NVIDIA's device-wide adapter plus the fields
   every vendor has (in-process sessions, Vulkan memory headroom, model, lost
   device) first; AMD and Intel driver fields join as their hardware tests
   prove them.
2. Parity first: publish only when all three vendors fill the same fields.
   No AMD or Intel driver reports device-wide codec sessions, so this blocks
   indefinitely.
3. Process scope only: report what this process knows, with no driver
   adapters, and leave device-wide data to the caller's NVML. Smallest, but
   reverses the 2026-10-05 decision that callers write no vendor code.

Open, API shape (recommended first):

1. A handle: `gpu::Monitor::new(&frame::vulkan::Device)`, then
   `monitor.snapshot()` returns a `gpu::Snapshot` whose values nest under
   `process` and `device`. The handle owns what polling needs between calls:
   NVML's init, the PMU counters, and the wedged-uevent listener.
2. A free function `gpu::snapshot(&frame::vulkan::Device)`. Simplest call
   site, but it must hide the same state in a process-global cache per device.
3. A provider trait the caller implements. No in-tree NVML, but every caller
   then picks and owns vendor backends, the vendor code the Goal rules out.

Test: a unit test of each source's parsing against recorded fixtures, and
ignored hardware tests in `just rs gpu`'s per-vendor branches (the recipe
#4975 added) that open an encoder and see
the session and memory counts move.

Public API: a device capacity and health snapshot in moq-video. Wire: none.

## Related

- [VA-API encodes an external Vulkan image](/quest/m2/vaapi-vulkan-import.md) - picks a render node by the same `dev_t`
- [Vulkan Video encode on AMD](/quest/m2/vulkan-encode.md) - the AMD backend whose sessions this counts
