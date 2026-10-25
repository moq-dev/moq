# [L] Studio program encode from the GPU

## Goal

`moq-studio`'s program reaches moq-video's encoder as a GPU `Surface`
wherever the backend accepts one, instead of the CPU readback every frame
takes today, and the studio is run, not only compiled, on macOS and Windows.

The Output panel already counts readback frames and time; once this lands it
shows readback only for a backend that cannot take a GPU surface.

## Plan

- **Zero-copy encode:** the compositor's program texture is allocated as an
  exportable image on the device the window and sources share, then handed to
  the encoder without leaving the GPU:
  - NVENC: a `moq_video::frame::vulkan` slot (opaque-fd memory and a timeline
    semaphore), the path the NVDEC to NVENC transcode already uses.
  - VAAPI: a DMA-BUF export of the same image.
  - VideoToolbox: an IOSurface-backed `CVPixelBuffer` the Metal texture aliases.
  - Media Foundation: a shared D3D11 texture.
  This reaches below `wgpu` (its `as_hal` escape hatch), so keep the export in
  moq-video or moq-scene, never in the app.
- **Platforms:** run the studio on macOS (camera, display, microphone, system
  audio) and Windows (camera, display, window, microphone), fixing what only
  shows up there.
- **Tests:** extend `just rs render-sw`'s end-to-end test (a red canvas
  published to a test relay and decoded) to assert no readback where the
  backend takes a surface.

## Related

- [Hardware validation](/quest/m3/video-hardware.md) - capture and zero-copy paths not yet run on real machines
