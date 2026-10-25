# [M] Zero-copy source import

## Goal

Captured and decoded frames reach `moq-studio`'s compositor as GPU textures
on the shared device, imported rather than uploaded: PipeWire and VA-API
DMA-BUFs on Linux, IOSurface-backed `CVPixelBuffer`s through Metal on Apple,
and D3D11 textures on Windows. A CPU upload remains only as a fallback,
counted per source and shown in the studio's stats.

## Plan

- moq-scene draws every frame through moq-video's `Renderer`, which already
  imports DMA-BUF (`render/dmabuf.rs`) and Metal (`render/metal.rs`) surfaces
  without a copy, and decoded broadcast sources already take the decoder's
  native surface. What keeps the studio on the CPU path today:
  - moq-scene's `capture` does not enable moq-video's `pipewire` feature
    (off by default because it links libpipewire), so Linux display and
    camera sources arrive as CPU frames the renderer uploads instead of
    PipeWire DMA-BUFs. Decide its build default with
    [packaging](/quest/m3/studio/packaging.md) in mind.
  - Windows has no D3D11 import yet: [Direct3D11 render import](/quest/m2/render-d3d11.md).
    Until it lands, Windows sources take the counted upload.
- The renderer reports whether a frame was imported or uploaded, so moq-scene
  counts uploads per source and the studio shows them next to readback. The
  still-image `queue.write_texture` in `rs/moq-scene/src/compositor.rs` runs
  once per image and is not part of this.
- Keep the import in moq-video or moq-scene, never in the app.
- **Tests:** the compositor's lavapipe tests assert that a DMA-BUF source
  draws without an upload, and the Metal path is asserted on macOS CI.

## Related

- [Zero-copy program encode](/quest/m2/studio-gpu/encode.md) - the other half of keeping the studio's pictures on the GPU
- [Validate PipeWire DMA-BUFs into the Vulkan renderer](/quest/m2/2819-moq-video-carry-pipewire-dma-bufs-safely-into-the-vulkan.md) - the hardware proof of the Linux import path
- [Direct3D11 render import](/quest/m2/render-d3d11.md) - the Windows import this needs
- [VAAPI encode and decode](/quest/m2/video-vaapi.md) - decoded DMA-BUF surfaces the renderer imports
- [Hardware validation](/quest/m3/video-hardware.md) - capture and zero-copy paths not yet run on real machines
