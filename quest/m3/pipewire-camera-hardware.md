# [S] Validate PipeWire cameras on a portal and a Pi

## Goal

The xdg-desktop-portal Camera path and a Raspberry Pi CSI camera each capture frames through the PipeWire camera that already shipped, or this quest records what stopped the pass. There is no new capture API.

## Plan

Open `pipewire` and one `pipewire:<node>` in a sandbox, where the portal raises its permission dialog, and on a Pi whose CSI camera is a PipeWire node (spa-libcamera). Record the mode that opened, whether frames arrived, and whether the producer used one memory block or one per plane.

Fix only a defect the pass hits. A separate-plane producer belongs to the multi-plane quest. If that is why a Pi produces nothing, write that down and stop. `doc/lib/rs/moq-video.md` says both paths are reachable. If a path cannot capture, correct that sentence in the same change.

A libcamera source stays out. Embedded video already leaves `rpicam-vid` to the application. This pass uses the PipeWire camera only.

## Required

- A sandbox that can show the camera portal dialog, and a Raspberry Pi whose CSI camera appears as a PipeWire node

## Related

- [Capture multi-plane PipeWire cameras](/quest/m2/pipewire-camera-planes.md) - separate-plane I420 and NV12, when the pass finds them
- [Video hardware validation](/quest/m3/video-hardware.md) - the other encode and capture runs that still need a machine
- [Embedded video path](/quest/m3/video-embedded.md) - presenting on a Pi, which is a different gap
