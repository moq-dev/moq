# [M] Video hardware validation

## Goal

The encode, capture, and zero-copy paths that were written but never run on
real hardware get run on it, and what breaks gets fixed.

## Plan

Every item here is blocked on a physical machine rather than on code, which is
why they sit together and why they sit in m3.

- **VAAPI low-power entrypoint and a second GPU.** H.264 encode, DMA-BUF
  input, and VPP resize ran on Intel Meteor Lake with iHD (moq-vaapi 0.1.0).
  Still unrun: the low-power encode entrypoint, which that device does not
  expose, and `MOQ_VAAPI_DEVICE` naming a node other than the first render
  node.
- **VAAPI input from V4L2 `VIDIOC_EXPBUF`.** DMA-BUF encode ran from a VA-API
  decode and from PipeWire. A V4L2 export has not been the source.
- **Windows Media Foundation capture**: on-demand open and close, so the
  camera LED is off when nobody is watching, and NV12 delivery from MJPEG and
  YUY2 cameras.
- **A live camera run per platform**: capture needs device permission that a
  headless or agent process cannot grant itself.

Precedent for what this catches: NVENC validation on an RTX 3070 Ti found that
NVENC rejects stream-ordered pool memory, so buffers registered with it must
come from plain `cuMemAlloc`. That is not a bug any amount of review finds.

## Related

- [Validate PipeWire cameras on a portal and a Pi](/quest/m3/pipewire-camera-hardware.md) - the camera portal and a Pi CSI node, which are a different machine from this list
- [PipeWire DMA-BUF on KDE](/quest/m3/2893-video-validate-pipewire-dma-buf-capture-on-kde-hardware.md) - the same kind of gate, for screen capture
