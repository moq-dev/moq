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
rather than copying, keeping the same per-path fallback and disable behavior
the other import paths use: a machine where the import cannot work should
quietly take the I420 route, not fail to present.

This is where the epic's "egress accessors for the other GPU variants as their
consumers land" comes due for `d3d11::Texture`: the renderer is that consumer,
so whatever access it needs becomes public here rather than ahead of it.

Worth doing on Windows specifically because 4K screen sharing is where the
download plus re-upload costs most, and Desktop Duplication makes that the
common case.

## Related

- [Video hardware validation](/quest/m3/video-hardware.md) - needs the same
  Windows machine
