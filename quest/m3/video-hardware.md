# [L] Video hardware validation

## Goal

The encode, capture, and zero-copy paths that were written but never run on
real hardware get run on it, and what breaks gets fixed.

## Plan

Every item here is blocked on a physical machine rather than on code, which is
why they sit together and why they sit in m3. Decided in the 2026-09-30 audit:
the KDE DMA-BUF capture and PipeWire camera validations fold in here, and the
V4L2 `VIDIOC_EXPBUF` source is dropped with the export itself (see
[#2819](/quest/m2/2819-moq-video-carry-pipewire-dma-bufs-safely-into-the-vulkan.md)).

- **VAAPI low-power entrypoint and a second GPU.** H.264 encode, DMA-BUF
  input, and VPP resize ran on Intel Meteor Lake with iHD (moq-vaapi 0.1.0).
  Still unrun: the low-power encode entrypoint, which that device does not
  expose, and `MOQ_VAAPI_DEVICE` naming a node other than the first render
  node.
- **Windows Media Foundation capture**: on-demand open and close, so the
  camera LED is off when nobody is watching, and NV12 delivery from MJPEG and
  YUY2 cameras.
- **A live camera run per platform**: capture needs device permission that a
  headless or agent process cannot grant itself.

### PipeWire DMA-BUF capture on KDE

Tracked in [#2893](https://github.com/moq-dev/moq/issues/2893). On a
KDE/Wayland desktop the portal source was selected, but the ignored
`portal_captures_frames` test never received a frame and timed out at 120
seconds:

```sh
cargo nextest run --profile ci -p moq-video --all-features --run-ignored ignored-only portal_captures_frames --no-capture
```

- Reproduce the post-selection timeout, then add tracing around portal
  completion, PipeWire connection, format fixation, buffer allocation, and
  first-frame delivery to locate the stall.
- A DMA-BUF-capable compositor produces `Surface::DmaBuf`; confirm the linear
  DMA-BUF CPU fallback and shared-memory capture still work when DMA-BUF is
  unavailable.
- Refine the ignored test so this failure is distinguishable from a
  portal-selection timeout.

### PipeWire cameras on a portal and a Pi

Open `pipewire` and one `pipewire:<node>` in a sandbox, where the portal
raises its permission dialog, and on a Raspberry Pi whose CSI camera is a
PipeWire node (spa-libcamera). Record the mode that opened, whether frames
arrived, and whether the producer used one memory block or one per plane.
No new capture API, and no libcamera source.

A separate-plane producer belongs to
[multi-plane cameras](/quest/m2/pipewire-camera-planes.md); if that is why a
Pi produces nothing, write it down and stop. `doc/lib/rs/moq-video.md` says
both paths are reachable; correct that sentence if one cannot capture.

### Fixes

Fix only a defect a run hits. Precedent for what this catches: NVENC
validation on an RTX 3070 Ti found that NVENC rejects stream-ordered pool
memory, so buffers registered with it must come from plain `cuMemAlloc`. That
is not a bug any amount of review finds.

## Required

- Someone with the hardware runs it: an Intel GPU exposing the VAAPI low-power
  entrypoint, a second render node, a Windows machine with MJPEG and YUY2
  cameras, a live camera per platform, a KDE/Wayland desktop with an Intel or
  AMD GPU, a sandbox that can show the camera portal dialog, and a Raspberry
  Pi whose CSI camera appears as a PipeWire node

## Closes

- [#2893](https://github.com/moq-dev/moq/issues/2893) - close this issue when the quest finishes

## Related

- [#2819](/quest/m2/2819-moq-video-carry-pipewire-dma-bufs-safely-into-the-vulkan.md) - the DMA-BUF import validation this capture feeds
- [Capture multi-plane PipeWire cameras](/quest/m2/pipewire-camera-planes.md) - separate-plane I420 and NV12, when the Pi pass finds them
- [Embedded video path](/quest/m3/video-embedded.md) - presenting on a Pi, which is a different gap
