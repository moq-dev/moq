# [S] moq-video: run the PipeWire DMA-BUF render path on a desktop GPU

## Goal

The Linux zero-copy spine from [#2819](https://github.com/moq-dev/moq/issues/2819),
`PipeWire DMA-BUF -> Surface::DmaBuf -> Vulkan import -> render shader`, has
been run on an Intel or AMD desktop, and the DRM modifiers its captures
arrive with are recorded.

## Plan

The code and its ignored hardware tests exist: packed capture through Vulkan
(`packed_dmabuf_renders_through_vulkan`, `rs/moq-video/src/render/renderer.rs`),
and NV12 and I420 import from VA-API surfaces
(`the_nv12_dmabuf_import_matches_the_cpu_path` and its siblings). A Hyprland
capture now delivers DMA-BUF frames
([moq-dev/moq#8579](https://github.com/moq-dev/moq/pull/8579)).

What remains:

- Run the ignored packed test on an Intel or AMD desktop and record the
  result in the PR, including that holding several frames never shows reused
  content or exhausts the PipeWire pool for good.
- Modifiers: `render/dmabuf.rs` refuses a buffer whose modifier the driver
  lists for neither format and re-tiles nothing. Log the modifiers each
  capture source arrives with. A VA-API VPP re-tile is only worth adding if a
  measured source lands on an unimportable one.

Decided in the 2026-09-30 audit: V4L2 `VIDIOC_EXPBUF` export is dropped.
V4L2 capture only takes YUYV and MJPEG (`rs/moq-video/src/capture/v4l2.rs`),
both of which need a CPU conversion or decode anyway, so exporting the buffer
saves no copy. Revisit only if NV12 camera capture lands.

Refs #2481, #1837.

## Related

- [Video hardware validation](/quest/m5/video-hardware.md) - owns the KDE capture stall and the shared-memory fallback check
