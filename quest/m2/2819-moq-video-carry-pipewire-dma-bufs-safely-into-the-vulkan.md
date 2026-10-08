# [M] moq-video: validate PipeWire DMA-BUFs into the Vulkan renderer

## Goal

The Linux zero-copy spine from [#2819](https://github.com/moq-dev/moq/issues/2819),
`PipeWire DMA-BUF -> Surface::DmaBuf -> Vulkan import -> render shader`, is
proven on real hardware.

## Plan

Built already: the `dmabuf` feature and `Surface::DmaBuf`, PipeWire DMA-BUF
negotiation with the shared-memory fallback, the dequeued buffer retained
until the last clone drops (#2839), packed RGB import in
`rs/moq-video/src/render/dmabuf.rs`, and NV12 import (#3331), which aliases
the buffer as an `R8` luma and an `RG8` chroma texture with no copy. VA-API
encode takes a `Surface::DmaBuf` directly.

What remains:

- Hardware gates, as ignored tests with a reason where CI lacks the device:
  native Linux `--features pipewire,render` tests; the shared-memory fallback
  still captures; packed and NV12 DMA-BUF capture renders with zero CPU
  download on an Intel or AMD desktop; holding several frames never shows
  reused content or exhausts the PipeWire pool for good.
- Modifier mismatch: `render/dmabuf.rs` downloads a buffer whose modifier the
  driver will not import, and re-tiles nothing. A VA-API VPP re-tile is only
  worth adding if a measured capture source lands on such a modifier; record
  the modifiers seen and decide.

Decided in the 2026-09-30 audit: V4L2 `VIDIOC_EXPBUF` export is dropped.
V4L2 capture only takes YUYV and MJPEG (`rs/moq-video/src/capture/v4l2.rs`),
both of which need a CPU conversion or decode anyway, so exporting the buffer
saves no copy. Revisit only if NV12 camera capture lands.

Refs #2481, #1837.

## Closes

- [#2819](https://github.com/moq-dev/moq/issues/2819) - close this issue when the quest finishes

## Related

- [Capture multi-plane PipeWire cameras](/quest/m3/pipewire-camera-planes.md) - separate memory blocks from a camera, the capture offer rather than this import
