# [S] MJPEG-only cameras show a correct picture

## Goal

A UVC camera that offers only MJPEG, such as a Sony ZV-E10 at 1280x720,
captures a correct picture through both the PipeWire and the V4L2 camera
paths. A test decoding a real frame from it covers the decode, and each
capture path is checked end to end with the camera on the maintainer's
desktop.

## Plan

Found 2026-11-25 in the studio on a Fedora desktop, the camera added through
PipeWire: its picture came out as a squashed top half over a blue-tinted
bottom half. Both paths decode with `I420::from_mjpeg` in
`rs/moq-video/src/frame.rs`, which has zune-jpeg decode to RGB and reads the
result as tightly packed 3-byte RGB. Its test covers only a 32x16 baseline
JPEG. Suspects, to settle from a real frame: zune-jpeg's output layout or
channel count for this camera's JPEG (4:2:2 chroma, restart markers), or
the PipeWire buffer handed to it.

Capture a few frames on the maintainer's desktop with the camera plugged in
and nothing else holding it
(`v4l2-ctl -d /dev/video0 --set-fmt-video=width=1280,height=720,pixelformat=MJPG --stream-mmap --stream-count=3 --stream-to=zv-e10.mjpeg`,
after `--get-fmt-video` confirms that format),
and keep one small frame as the test's fixture. Fix at the root, ideally
decoding straight to YUV rather than through RGB.

## Hosts

- linux - the camera is on the maintainer's Fedora desktop
