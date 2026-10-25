# [M] Direct3D11 render import

## Goal

Windows presents decoded frames without a round trip through system memory.
Decode already keeps its DXVA texture and can resize it on the GPU, but the
renderer has no Direct3D11 import, so every presented frame is downloaded to
I420 and re-uploaded.

## Plan

The surface is already there and already GPU-resident: `Surface::Texture`
carries a D3D11 NV12 texture, decode retains it, and GPU resize works. What is
missing is the import at the other end, which the renderer has for Vulkan
DMA-BUF and Metal but not here, so Windows falls through to the CPU path.

Import the texture as a wgpu texture through its D3D12 or Vulkan backend
rather than copying. Like the other import paths, the renderer has no
fallback: until this lands it refuses `Surface::Texture`, and
`Output::Render` downloads those frames at the decoder or capture instead.
Landing it adds `Surface::Texture` to what `Output::Render` keeps on the GPU.

The renderer is the first consumer of `d3d11::Texture`'s contents outside
moq-video's frame module (its fields are `pub(crate)`,
`rs/moq-video/src/frame.rs`), so whatever access it needs becomes public here
rather than ahead of it.

Worth doing on Windows specifically because 4K screen sharing is where the
download plus re-upload costs most, and WGC screen capture makes that the
common case.

## Related

- [Video hardware validation](/quest/m3/video-hardware.md) - needs the same
  Windows machine
