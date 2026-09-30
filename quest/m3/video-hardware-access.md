# [XS] Video validation hardware is on hand

## Goal

Someone with the hardware can run the video validation: an Intel GPU
exposing the VAAPI low-power entrypoint, a second render node, a Windows
machine with MJPEG and YUY2 cameras, a live camera per platform, a KDE/Wayland
desktop with an Intel or AMD GPU, a sandbox that can show the camera portal
dialog, and a Raspberry Pi whose CSI camera appears as a PipeWire node.

This quest tracks a condition outside the repository. When it holds, delete
this quest and every `Required` entry that links it.

## Plan

The validation can start with whatever subset is available; split the rest
out of [Video hardware validation](/quest/m3/video-hardware.md) when that
happens.
