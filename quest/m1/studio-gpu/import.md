# [M] Zero-copy source import

## Goal

Captured and decoded frames reach `moq-studio`'s compositor as GPU textures
on the shared device, imported rather than uploaded, on Linux (PipeWire and
VA-API DMA-BUFs) and Apple (IOSurface-backed `CVPixelBuffer`s through Metal).
moq-video's renderer reports whether each frame was imported or uploaded, and
the studio counts uploads per source next to its readback stats.

## Plan

- moq-scene draws every frame through moq-video's `Renderer`, which already
  imports DMA-BUF (`render/dmabuf.rs`) and Metal (`render/metal.rs`) surfaces
  without a copy, and decoded broadcast sources already take the decoder's
  native surface. What remains:
  - The renderer API reports import or upload per frame, so a caller can see
    its fallback. This is the moq-video API change the cut should carry.
  - moq-scene's `capture` does not enable moq-video's `pipewire` feature
    (off by default because it links libpipewire), so Linux display and
    camera sources arrive as CPU frames. Decide its build default with
    [packaging](/quest/m3/studio/packaging.md) in mind.
- Windows sources keep the counted upload until
  [Direct3D11 render import](/quest/m2/render-d3d11.md) lands in m2.
- The still-image `queue.write_texture` in `rs/moq-scene/src/compositor.rs`
  runs once per image and is not part of this.
- Keep the import in moq-video or moq-scene, never in the app.
- **Tests:** the compositor's lavapipe tests assert the upload count for a CPU
  frame, macOS CI asserts a Metal import, and a PipeWire DMA-BUF import is an
  ignored hardware test.

## Related

- [Validate PipeWire DMA-BUFs into the Vulkan renderer](/quest/m2/2819-moq-video-carry-pipewire-dma-bufs-safely-into-the-vulkan.md) - the hardware proof of the Linux import path
- [Direct3D11 render import](/quest/m2/render-d3d11.md) - the follow-up that brings Windows sources onto the GPU
- [VAAPI encode and decode](/quest/m2/video-vaapi.md) - decoded DMA-BUF surfaces the renderer imports
- [Hardware validation](/quest/m3/video-hardware.md) - capture and zero-copy paths not yet run on real machines
