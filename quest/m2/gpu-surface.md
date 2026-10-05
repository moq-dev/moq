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
  - the handle type: `OPAQUE_FD`, or `DMA_BUF` with a DRM format modifier;
  - the format and size;
  - the device and driver UUID;
  - the timeline: the value the producer signals when the image is ready and
    the value the consumer signals when it is done reading.
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

Open, found in review:

- Where `Kind::Auto` learns the device. `Encoder::new` opens its backend
  eagerly from `Config` alone, and `Config::probe` feeds a throwaway encoder a
  synthetic I420 frame. So no surface exists at selection time, and on a host
  with two GPUs `Auto` would open whichever backend comes first in
  `HARDWARE`. Options:
  - a `Config` field naming the input device by device and driver UUID
    (recommended: `probe` and fail-fast open keep working);
  - deferring the open to the first frame, which breaks `probe`'s
    advertise-before-the-first-frame contract.

  Either is a public API change, so record it here. The refusal test must
  cover `Encoder::new` and `probe` as well as a frame.
- Slot identity. Today a `Slot` exists only through the CUDA
  `vulkan::Importer::import`, and `Slot::publish` and `Completion` wait on
  CUDA. Lift slot identity and completion out of the CUDA importer so a
  producer publishes a slot before any backend has imported it. Each backend
  caches its import by that slot identity, never by fd, since a fresh or
  dup'd fd cannot identify a slot.
- `DmaBuf::new` takes an `Arc<dyn DmaBufFrame>`, a trait kept `pub(crate)` so
  backend lifetimes stay private. Decide what an external producer passes
  instead (an `OwnedFd` plus a release guard, say), and refuse
  `download_i420` on an external buffer as `Surface::Vulkan` already refuses
  readback.

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
