# [L] Zero-copy program encode

## Goal

`moq-studio`'s program reaches moq-video's encoder as a GPU `Surface`
wherever the backend accepts one, instead of the CPU readback every frame
takes today.

The Output panel already counts readback frames and time; once this lands it
shows readback only for a backend that cannot take a GPU surface.

## Plan

- The compositor's program texture is allocated as an exportable image on
  the device the window and sources share, then handed to the encoder
  without leaving the GPU. Each encoder below already accepts its surface;
  the work is the export:
  - NVENC: a `moq_video::frame::vulkan` slot (opaque-fd memory and a timeline
    semaphore), the path the NVDEC to NVENC transcode already uses.
  - VAAPI: a DMA-BUF export of the same image. Its explicit sync is
    [VA-API encodes an external Vulkan image](/quest/m2/vaapi-vulkan-import.md);
    until that lands, Intel keeps the counted readback.
  - VideoToolbox: an IOSurface-backed `CVPixelBuffer` the Metal texture aliases.
  - Media Foundation: a shared D3D11 texture.
  - AMD: [Vulkan Video encode](/quest/m2/vulkan-encode.md) takes the same
    Vulkan slot once it lands.
- This reaches below `wgpu` (its `as_hal` escape hatch), so keep the export in
  moq-video or moq-scene, never in the app.
- **Tests:** extend `just rs render-sw`'s end-to-end test (a red canvas
  published to a test relay and decoded) to assert no readback where the
  backend takes a surface. The VideoToolbox and Media Foundation exports are
  tested in moq-video or moq-scene on their platforms' CI.

## Related

- [Zero-copy source import](/quest/m2/studio-gpu/import.md) - the other half of keeping the studio's pictures on the GPU
- [VA-API encodes an external Vulkan image](/quest/m2/vaapi-vulkan-import.md) - the explicit-sync DMA-BUF path Intel needs
- [Vulkan Video encode on AMD](/quest/m2/vulkan-encode.md) - the AMD backend for the same Vulkan slot
- [NVENC reuses its input and output buffers](/quest/m2/nvenc-pool.md) - per-frame NVENC allocation the program encode also pays
- [VAAPI encode and decode](/quest/m2/video-vaapi.md) - the VA-API encoder this hands a DMA-BUF
- [Hardware validation](/quest/m3/video-hardware.md) - capture and zero-copy paths not yet run on real machines
