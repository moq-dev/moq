# [M] Video hardware validation

## Goal

The encode, capture, and zero-copy paths that were written but never run on
real hardware get run on it, and what breaks gets fixed.

## Plan

Every item here is blocked on a physical machine rather than on code, which is
why they sit together and why they sit in m3.

- **VAAPI encode on an Intel or AMD box**: low-power against full entrypoint,
  the NV12 upload round trip, and `cargo deny` license resolution.
- **VAAPI zero-copy dmabuf input**: the backend uses an NV12 surface upload
  today. Exercise the `Surface::DmaBuf` path with a V4L2 `VIDIOC_EXPBUF`
  source instead.
- **Windows Media Foundation capture**: on-demand open and close, so the
  camera LED is off when nobody is watching, and NV12 delivery from MJPEG and
  YUY2 cameras.
- **A live camera run per platform**: capture needs device permission that a
  headless or agent process cannot grant itself.

Precedent for what this catches: NVENC validation on an RTX 3070 Ti found that
NVENC rejects stream-ordered pool memory, so buffers registered with it must
come from plain `cuMemAlloc`. That is not a bug any amount of review finds.

## Related

- [PipeWire DMA-BUF on KDE](/quest/m3/2893-video-validate-pipewire-dma-buf-capture-on-kde-hardware.md) - the same kind of gate, for the capture side
