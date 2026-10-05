# [XL] Vulkan Video encode on AMD

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
- Hand-rolled on ash, about 1.5-2k lines of Vulkan Video. Rejected:
  - patching gpu-video upstream. It creates its own `VkDevice`, enables no
    external semaphore, and has no import API.
  - forking gpu-video into moq-dev.
  - pixelforge, which needs git ash and is not on crates.io.

  Why: moq-video owns device creation, so it enables
  `VK_KHR_external_memory_fd` and `VK_KHR_external_semaphore_fd` itself. That
  lets it import the producer's `OPAQUE_FD` BGRA image as a dedicated
  allocation with create info identical to the exporter's. It also leaves
  intra refresh and AV1 possible later.
- H.264 and H.265. AV1 is out of scope.
- The bar is no CPU round trip; GPU-side copies are allowed.

Building it:

- Device: pick the physical device whose device and driver UUID match the
  surface. Enable the video encode queue, H.264 and H.265 encode, external
  memory, and external semaphore extensions. Import the timeline and wait on
  the producer's ready value on the GPU. Signal its done value once the
  encode has read the image, so slots recycle.
- Input: one compute pass reads the imported BGRA image and writes each
  rendition's NV12 encode input picture, converting and scaling in one step.
- Low latency is reachable with:
  - CBR with `virtualBufferSizeInMs`;
  - tuning mode `LOW_LATENCY` or `ULTRA_LOW_LATENCY`;
  - `consecutiveBFrameCount` 0;
  - IDRs placed by the app, for `Encoder::cut` and `Gop`.
- ash: crates.io's newest is 0.38 (Vulkan 1.3.281), checked 2026-10-05.
  - It carries the final `VK_KHR_video_encode_queue`, `_h264`, `_h265`, and
    `VK_KHR_video_maintenance1`, with the tuning, buffer, and B-frame fields
    above.
  - It lacks `VK_KHR_video_encode_intra_refresh`, `_quantization_map`, and
    `_av1`. Those need a newer ash and are out of scope; refresh mode is
    refused on this backend.
  - moq-video uses ash 0.38 only as a dev-dependency (the Vulkan/CUDA test
    producer, through the workspace's `ash = "0.38"`). wgpu-hal 30, under
    `render`, depends on the same 0.38. Make it a normal optional dependency
    of this backend's feature.
- Packaging: Fedora's stock Mesa omits the H.264 and H.265 encoders. A device
  without the extensions is refused with an error naming them, and the
  hardware recipe reports them as missing instead of failing obscurely.

Test: an ignored hardware test in `just rs gpu`'s AMD branch renders on one
`VkDevice` and exports `OPAQUE_FD` memory and a timeline. It encodes H.264
and H.265 at two sizes, checks that the output decodes, and checks that the
slots recycle. Gate the backend behind its own feature. Turn it on by default
only if it costs nothing beyond cargo, per the rule in
`rs/moq-video/Cargo.toml`.

Public API: a new encoder backend and feature. Wire: none.

## Required

- [One external GPU image for every encoder](/quest/m2/gpu-surface.md) - the surface this backend imports

## Related

- [VA-API encodes an external Vulkan image](/quest/m2/vaapi-vulkan-import.md) - the Intel half of the same proof
- [Intra-refresh GOPs](/quest/m2/intra-refresh/README.md) - refresh mode this backend refuses until ash carries the extension
