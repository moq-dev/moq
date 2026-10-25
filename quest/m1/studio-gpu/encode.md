# [L] Zero-copy program encode

## Goal

`moq-studio`'s program reaches moq-video's encoder as a GPU `Surface` on
NVENC, VideoToolbox, and Media Foundation, instead of the CPU readback every
frame takes today. Any other backend keeps the readback, which the Output
panel already counts.

## Plan

- The compositor's program texture is allocated as an exportable image on
  the device the window and sources share, then handed to the encoder
  without leaving the GPU. Each encoder below already accepts its surface;
  the work is the export and the API that carries it:
  - NVENC: a `moq_video::frame::vulkan` slot (opaque-fd memory and a timeline
    semaphore), the path the NVDEC to NVENC transcode already uses.
  - VideoToolbox: an IOSurface-backed `CVPixelBuffer` the Metal texture aliases.
  - Media Foundation: a shared D3D11 texture.
- This reaches below `wgpu` (its `as_hal` escape hatch), so keep the export in
  moq-video or moq-scene, never in the app.
- Shape the export so the Vulkan slot also serves VA-API and AMD Vulkan
  Video later without an API change; wiring those backends is m2 work.
- **Tests:** extend `just rs render-sw`'s end-to-end test (a red canvas
  published to a test relay and decoded) to assert the readback count, which
  is nonzero there. VideoToolbox and Media Foundation assert no readback on
  their platforms' CI, and NVENC in an ignored hardware test run by
  `just rs gpu`.

## Related

- [VA-API encodes an external Vulkan image](/quest/m2/vaapi-vulkan-import.md) - the follow-up that gives Intel the same Vulkan slot
- [Vulkan Video encode on AMD](/quest/m2/vulkan-encode.md) - the follow-up that gives AMD the same Vulkan slot
- [NVENC reuses its input and output buffers](/quest/m2/nvenc-pool.md) - per-frame NVENC allocation the program encode also pays
- [Hardware validation](/quest/m3/video-hardware.md) - capture and zero-copy paths not yet run on real machines
