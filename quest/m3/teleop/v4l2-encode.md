# [L] V4L2-M2M encoding

## Goal

`moq-video` encodes in hardware on the boards that fly, so `moq-cli` needs no
GStreamer detour on a Raspberry Pi, and the hardware worth buying is written
down.

## Plan

Add a V4L2-M2M stateful encoder backend next to openh264, NVENC, VAAPI,
VideoToolbox and MediaFoundation, pairing with the V4L2 capture path that
already exists. Mainline kernel API, no vendor SDK. It covers Pi 4, CM4 and
Zero 2 W via `bcm2835-codec`, which is what the drone world actually flies.

Rockchip is deliberately excluded: RK3588 encoding goes through rkmpp in a
vendor kernel rather than V4L2, and the existing `moq-gst` route already ships
aarch64 packages for it. Adding an rkmpp backend is a separate decision.

Two landmines make this worth owning, and make the hardware note part of the
deliverable rather than an afterthought: Raspberry Pi 5 has no video encoder at
all, and Jetson Orin Nano ships without NVENC. The boards that still encode are
Pi 4/CM4/Zero 2 W, Orin NX and above, and RK3588.

Released `moq-cli` binaries are built with default features, so `capture` is
not compiled in. A hardware encoder nobody can install is not a fix; settle how
the capture-enabled build ships as part of this quest.

## Related

- [vaapi](/quest/m3/teleop/vaapi.md) - the other unproven encoder backend
