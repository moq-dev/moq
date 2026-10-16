# [XS] Video validation hardware is on hand

## Goal

Someone with the hardware can run the video validation: a Windows machine
with MJPEG and YUY2 cameras, a live camera per platform, a KDE/Wayland desktop
with an Intel or AMD GPU, a sandbox that can show the camera portal dialog,
and a Raspberry Pi 4 or 5 whose CSI camera appears as a PipeWire node, which
also validates the [embedded video path](/quest/m3/video-embedded.md).

This quest tracks a condition outside the repository. When it holds, delete
this quest and every `Required` entry that links it.

## Plan

Check by asking the maintainer which machines are available. As of
2026-10-05 only part is on hand. The maintainer's desktop has an Intel Arrow
Lake iGPU (iHD 26.1.2, `renderD128`) and an AMD RX 9070 (RADV, Mesa 26.0.8,
`renderD129`), so it has a second render node; that check moved to
[VA-API external images](/quest/m2/vaapi-vulkan-import.md). The
[multi-vendor GPU quests](/quest/m2/gpu-release.md) test on it.

Checked 2026-10-08 on that desktop: `vainfo` lists only `VAEntrypointEncSlice`
on the iGPU (H.264, HEVC, HEVC Main10, AV1), no `EncSliceLP`, so the
low-power-only fallback is unreachable there and is no longer a validation
item. The desktop runs GNOME, not KDE, and has no camera (`/dev/video*` is
empty).

Decided 2026-10-08: the embedded-device condition folds in here, since a Pi
4 or 5 serves both the PipeWire camera pass and the embedded video check.
Record the device, OS image, and driver when one is on hand.

The validation can start with whatever subset is available; split the rest
out of [Video hardware validation](/quest/m3/video-hardware.md) when that
happens.
