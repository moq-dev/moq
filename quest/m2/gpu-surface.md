# [M] One external GPU image for every encoder

## Goal

A producer hands moq-video one vendor-neutral `Surface::Vulkan`: an image it
rendered and exported from any Vulkan device, with its timeline. `Kind::Auto`
opens the first encoder backend that can import it on that device, so the
caller holds no vendor code: no CUDA, no backend name, no vendor ID. NVENC
imports the image into CUDA itself, and its output on NVIDIA is unchanged.
`DmaBuf` gets a public constructor, so a producer that only has a DMA-BUF can
reach VA-API too.

This is the API half of proving moq-video carries a GPU frame from an external
producer to any vendor's encoder without a CPU round trip. The first producer
is moq.pro's CARLA bridge (a Vulkan plugin exporting `OPAQUE_FD` images and
timelines), which today names `nvenc`, builds `Surface::Cuda`
itself, and drives `vulkan::Importer` and `cuda::Converter`.

## Plan

Decided 2026-10-05 in a `/quest-plan` interview (paper trail in the PR that
added this quest):

- The surface is a neutral external image. Rejected: an encoder-owned input
  the producer renders into (it flips the producer's protocol), and offering
  both. `Surface::Vulkan` is no longer gated on `nvidia` and describes:
  - the memory handle: `OPAQUE_FD`, or `DMA_BUF` with a DRM format modifier
    and explicit memory-plane offsets and row pitches. Importers check the
    plane count against modifier properties and never infer tight rows;
  - the format, size, and allocation size. The image creation contract is 2D,
    one mip/layer/sample, flags zero, EXCLUSIVE sharing, and
    `TRANSFER_DST | SAMPLED | STORAGE` usage, with dedicated memory bound at
    offset zero. `OPAQUE_FD` uses optimal tiling and `DMA_BUF` uses explicit
    modifier tiling. For `OPAQUE_FD`, the exporter's
    memory type index too: a Vulkan import must reuse both (VUID 01742),
    and an `OPAQUE_FD` handle cannot be queried for its properties. A
    `DMA_BUF` importer picks its memory type from the handle's properties;
  - the device and driver UUID, and the render node's `dev_t` (decided
    2026-10-05: the one device identity
    [VA-API import](/quest/m2/vaapi-vulkan-import.md) and
    [GPU health](/quest/m2/gpu-health.md) key by). A device without
    `VK_EXT_physical_device_drm` or a render node carries none, and a
    backend that needs it refuses the surface rather than guess;
  - the timeline: an `OPAQUE_FD` timeline semaphore handle, the value the
    producer signals when the image is ready, and the value the consumer
    signals when it is done reading. Both handles belong to the slot and live
    as long as it does, as `vulkan::Handles` do today.
- Each backend imports the image itself: CUDA for NVENC,
  [Vulkan for RADV](/quest/m2/vulkan-encode.md), and
  [VA-API through a DMA-BUF](/quest/m2/vaapi-vulkan-import.md). The slot and
  completion recycling in `frame/vulkan.rs` stays: a slot returns to the
  producer only after the encoder's GPU work on it has finished.
- Selection is automatic by the surface's device. `Kind::Auto` opens the first
  backend that can import the surface on its device, matched by device and
  driver UUID. `Kind::Named` stays for overrides and tests. Rejected: the
  caller names the backend.
- The bar is no CPU round trip. Extra GPU-side copies are allowed, such as a
  conversion into the encoder's own NV12 input. A strict direct RGB encode is
  not required.
- Per-rendition scaling stays on the GPU: one render feeds several encoders at
  different sizes (moq.pro encodes 426x240 and 1280x720 from one 1280x720
  render). Keep NVIDIA at one conversion per capture, or measure what a second
  one costs.
- Docs are rustdoc, inline. No doc.moq.dev page.

Implementation decisions:

- Device selection is decided: `encode::Config::input` names the external
  device by device and driver UUID and optional render-node `dev_t`. The encoder
  opens eagerly, and `probe` can advertise before capture begins. A configuration
  naming an external device cannot fall back to software, and an external frame
  on another device is refused before reaching the backend.
- `vulkan::Slot::new` owns the exported handles and producer guard without
  opening CUDA. Each slot caches imports by backend type on its allocation,
  rather than an FD number. Imports live with the producer-owned allocation,
  without an additional backend quota on the number of producer slots.
  Published clones share one reader and per-color
  NV12 conversions. Different import backends for one published image are
  refused until there is a protocol for ordering their completion signals.
- `DmaBuf::new` takes an `OwnedFd`, a `DmaBufLayout`, and a producer release
  guard. Its private adoption path keeps capture and decoder lifetimes private.
  External buffers refuse CPU download even when the allocation is linear.
- An unconsumed external Vulkan frame fails completion and releases the slot;
  it cannot be recycled because no backend signalled its completion timeline.
  The producer guard owns safe teardown of its allocation.

NVIDIA hardware verification remains required before this quest is complete:
run `just rs gpu` on the NVIDIA demo PC. The AMD/Intel development PC can compile
these tests and exercise refusal/lifetime policy, but cannot prove NVENC output
or CUDA external-image import.

What moves: `vulkan::Importer::new(ordinal, capacity)`, `cuda::Converter`, and the
caller's `Surface::Cuda` construction go behind the NVENC backend for this
path. Keep public only what another consumer still needs (NVDEC output stays
`Surface::Cuda`). This is a breaking change on `main`; see
[the release quest](/quest/m2/gpu-release.md) for how it ships.

Tests:

- Unit-test selection without a GPU: a device no backend can import is
  refused with an error naming the device, never a CPU fallback, at
  `Encoder::new` and `probe` as well as at encode.
- The existing `vulkan_cuda_` hardware tests move onto the new surface,
  `#[ignore = "requires ..."]`, following [GPU CI](/quest/m1/gpu-ci.md)'s
  convention.
- Add one recipe, `just rs gpu`, that detects the host's GPUs by PCI vendor
  and runs the ignored tests for each vendor present, and fails when a
  detected GPU's driver is missing instead of skipping. `just rs vulkan-cuda`
  folds into its NVIDIA branch, which GPU CI's `just rs nvidia` selection
  later becomes. The other quests add their vendor's tests to it.

Public API: `Surface::Vulkan` changes shape and loses its `nvidia` gate;
`DmaBuf::new` becomes public; the CUDA import types leave the caller's path.
Wire: none.

## Related

- [GPU CI](/quest/m1/gpu-ci.md) - the NVIDIA selection and the ignored-test convention
- [Linux OBS GPU input](/quest/m3/obs-linux-gpu.md) - another external producer, which hands over a DMA-BUF
