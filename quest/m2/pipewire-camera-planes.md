# [M] Capture multi-plane PipeWire cameras

## Goal

A PipeWire camera that delivers I420 or NV12 in separate memory blocks produces frames. Single-block cameras keep working. Other pixel formats stay unsupported. Importing multi-plane NV12 into Vulkan stays with the PipeWire DMA-BUF quest.

## Plan

The buffer offer sets `SPA_PARAM_BUFFERS_blocks` to 1, so a producer that puts each plane in its own block never links. Offer enough blocks for the format, two for NV12 and three for I420, and map each block into the frame the CPU path already converts. A one-block buffer keeps the current mapping.

Unit-test the offer and a multi-block NV12 and I420 buffer with no camera attached. `doc/lib/rs/moq-video.md` already says a Pi CSI camera and a sandboxed camera are reachable. This quest is what makes the separate-plane case of that sentence true. No new page.

## Related

- [Validate PipeWire cameras on a portal and a Pi](/quest/m3/pipewire-camera-hardware.md) - the pass that shows whether a real Pi or portal camera delivers separate planes
- [PipeWire DMA-BUFs into Vulkan](/quest/m2/2819-moq-video-carry-pipewire-dma-bufs-safely-into-the-vulkan.md) - multi-plane NV12 import in the renderer
