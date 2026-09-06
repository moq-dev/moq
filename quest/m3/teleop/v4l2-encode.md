# [S] V4L2-M2M encoding

## Goal

A released `moq-cli` encodes in hardware on the boards that fly, so a Raspberry
Pi needs no GStreamer detour, and the hardware worth buying is written down.

## Plan

The backend exists: `moq-video`'s opt-in `v4l2` feature drives the stateful
V4L2 M2M encoder and decoder through `rs/moq-video/src/v4l2.rs`, and both have
run on a Pi 4 (`bcm2835-codec`). What is left is getting it into people's
hands.

Released `moq-cli` binaries are built with default features, so neither
`capture` nor `v4l2` is compiled in and the backend reaches nobody who installs
a release. A hardware encoder nobody can install is not a fix. [CLI
packaging](/quest/m2/cli-packaging.md) makes capture available; this quest must
also enable `v4l2` in the Linux ARM release build. It finishes once a released
binary on a Pi 4 publishes from `moq import capture` through the hardware
encoder.

Write the hardware note down, in `doc/bin/cli.md` next to the capture build
instructions. Two landmines make it part of the deliverable: Raspberry Pi 5 has
no video encoder at all, and Jetson Orin Nano ships without NVENC. The boards
that still encode are Pi 4/CM4/Zero 2 W, Orin NX and above, and RK3588.
Rockchip stays excluded from the backend: RK3588 encoding goes through rkmpp in
a vendor kernel rather than V4L2, and the existing `moq-gst` route already ships
aarch64 packages for it. Adding an rkmpp backend is a separate decision.

Validate what the Pi 4 run did not reach: `set_bitrate` on a running encoder
(congestion control retunes through it), resolutions past 640x360 where 1080p
codes as 1088 rows and the compose rectangle crops it back, and the other
`bcm2835-codec` boards (Zero 2 W, CM4).

## Required

- [CLI packaging](/quest/m2/cli-packaging.md) - the released binary has to
  compile `capture` before a hardware encoder in it reaches anyone

## Related

- [Video hardware validation](/quest/m3/video-hardware.md) - the other unproven encoder backend, VAAPI
