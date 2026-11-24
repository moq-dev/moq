# [L] Video hardware validation

## Goal

The encode, capture, and zero-copy paths that were written but never run on
real hardware get run on it, and what breaks gets fixed.

## Plan

Every item here is blocked on a physical machine rather than on code, which is
why they sit together and why they sit in m5. Decided in the 2026-09-30 audit:
the KDE DMA-BUF capture and PipeWire camera validations fold in here, and the
V4L2 `VIDIOC_EXPBUF` source is dropped with the export itself (see
[#2819](/quest/m4/2819-moq-video-carry-pipewire-dma-bufs-safely-into-the-vulkan.md)).

Decided 2026-10-08: the VAAPI low-power entrypoint check is dropped. Neither
Meteor Lake nor the Arrow Lake test host exposes `EncSliceLP`, so no machine
on hand reaches that fallback. A render node other than the first is checked
by [VA-API external images](/quest/m5/vaapi-vulkan-import.md).

Decided 2026-11-15 (maintainer, /quest-complete): ✅ split the items the
`windows` host (RTX 2070, no camera) can run today, the Windows GPU probes and
the two dx12 export tests, into [Windows GPU tests](/quest/m4/windows-gpu-tests.md)
(rejected: splitting them into the lab; keeping everything here until the
whole hardware set is on hand).

- **Windows Media Foundation capture**: the camera opens while capture is
  enabled and closes when it is disabled, so the LED is off whenever capture
  is; with the `on_demand` opt-in it also opens and closes as the first viewer
  arrives and the last leaves. NV12 delivery from MJPEG and YUY2 cameras,
  and the ignored `mediafoundation_camera_texture` encode test, which needs
  a camera and a GPU.
- **A GPU export on a Mac**: the [studio](https://github.com/moq-dev/studio),
  the export's main consumer, confirms in its own repository that its Output
  panel reports no readback on an Apple Silicon Mac.
- **A live camera run per platform**: capture needs device permission that a
  headless or agent process cannot grant itself.
- **OBS follows its grant on a shaped uplink**: stream from a real OBS build
  through `tc netem` (or `rs/moq-shaper`), narrow and reopen the link, and
  confirm the Stats tab's video target drops at once, climbs back over a few
  seconds, and the encoder output follows (the grant handling in
  `cpp/obs/src/moq-output.cpp`).

### PipeWire DMA-BUF capture on KDE

Reported in [#2893](https://github.com/moq-dev/moq/issues/2893). On a
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

A separate-plane producer is not planned; if that is why a
Pi produces nothing, write it down and stop. `doc/lib/rs/moq-video.md` says
both paths are reachable; correct that sentence if one cannot capture.

### Fixes

Fix only a defect a run hits. Precedent for what this catches: NVENC
validation on an RTX 3070 Ti found that NVENC rejects stream-ordered pool
memory, so buffers registered with it must come from plain `cuMemAlloc`. That
is not a bug any amount of review finds.

## Required

- [Video validation hardware is on hand](/quest/m5/video-hardware-access.md) - the machines to run it on

## Related

- [Windows GPU tests](/quest/m4/windows-gpu-tests.md) - the Windows GPU probes and dx12 export tests split out to run on the `windows` host
- [#2819](/quest/m4/2819-moq-video-carry-pipewire-dma-bufs-safely-into-the-vulkan.md) - the DMA-BUF import validation this capture feeds
- [Embedded video path](/quest/m5/video-embedded.md) - presenting on a Pi, which is a different gap
