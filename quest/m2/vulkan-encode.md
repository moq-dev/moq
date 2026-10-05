# [L] Vulkan Video encode on AMD

## Goal

moq-video encodes an external [`Surface::Vulkan`](/quest/m2/gpu-surface.md)
with Vulkan Video on AMD (RADV), in H.264 and H.265, scaled per rendition on
the GPU, with no CPU round trip. An ignored hardware test proves it on an
RX 9070, and `Kind::Auto` picks this backend for a surface on an AMD device
with no caller change.

Non-goal: running CARLA itself on AMD.

## Plan

Decided 2026-10-05 in a `/quest-plan` interview (paper trail in the PR that
added this quest):

- Vulkan Video, not VA-API on radeonsi. RADV on RDNA4 exposes
  `VK_KHR_video_encode_h264`, `_h265`, and `_av1` by default (Mesa 25.2+,
  verified on an RX 9070 with Mesa 26.0.8). An `OPAQUE_FD` image exported by
  another `VkDevice` on the same driver and device UUID imports into ours.
  VA-API on radeonsi would need a DMA-BUF export plus explicit sync, and one
  report says AMD's VA encoder refuses external input.
- Built on the [gpu-video](https://crates.io/crates/gpu-video) crate (Software
  Mansion, MIT, formerly vk-video): H.264 and H.265 encode, CBR, forced IDR, an
  RGBA-to-NV12 wgpu compute helper, and a shared wgpu device. Rejected:
  hand-rolling on ash through wgpu-hal (about 1.5-2k lines to own), and
  pixelforge (git ash only, not on crates.io). No intra refresh; refresh mode
  is refused on this backend, which is acceptable.
- H.264 and H.265. AV1 is out of scope.
- The bar is no CPU round trip; GPU-side copies are allowed.

How the import works, verified 2026-10-05 against gpu-video 0.4.0 and master:

- gpu-video creates its own `VkDevice`
  (`VideoAdapterExt::request_device_with_video_support`), but the caller picks
  the `wgpu::Adapter`: match the producer's device and driver UUID through
  `adapter.as_hal::<Vulkan>()` and `VkPhysicalDeviceIDProperties`.
- wgpu-hal 30 enables `VK_KHR_external_memory_fd` when the driver supports it,
  so the image imports: a dedicated allocation with create info identical to
  the exporter's (BGRA8, as rendered), wrapped with `texture_from_raw` and
  `create_texture_from_hal`. A compute pass writes the encoder's own NV12
  input from it, so the encode profile never has to be on the producer's
  image. Scaling per rendition happens in that pass.
- The timeline cannot import today: nothing enables
  `VK_KHR_external_semaphore_fd` on gpu-video's device, and its device
  descriptor has no extension hook. Once imported, the wait stays on the GPU
  through the hal queue's `add_wait_semaphore`.
- Open, for the maintainer: patch gpu-video upstream (an extra device
  extensions field, about 10 lines), fork it into moq-dev, or hand-roll on
  ash. A CPU wait on a helper device on the same GPU (`vkWaitSemaphores`)
  works without a patch, at one host sync per frame.
- gpu-video 0.4.0 is on wgpu 29 while moq-video's `render` is on wgpu 30.
  Master is on wgpu 30 with an unreleased API overhaul (#2039), so build
  against the release that follows it.

Test: an ignored hardware test in `just rs gpu`'s AMD branch renders on one
`VkDevice`, exports `OPAQUE_FD` memory and timeline, and encodes H.264 and
H.265 at two sizes; the output decodes, and the slots recycle. Gate the
backend behind its own feature; default-on only if it costs nothing beyond
cargo, per the rule in `rs/moq-video/Cargo.toml`.

Public API: a new encoder backend and feature. Wire: none.

## Required

- [One external GPU image for every encoder](/quest/m2/gpu-surface.md) - the surface this backend imports
- [gpu-video releases on wgpu 30](/quest/m2/gpu-video-wgpu30.md) - the crate release this builds on

## Related

- [VA-API from an external Vulkan image](/quest/m2/vaapi-vulkan-import.md) - the Intel half of the same proof
- [Intra-refresh GOPs](/quest/m2/intra-refresh/README.md) - refresh mode this backend refuses
