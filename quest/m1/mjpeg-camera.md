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

Checked 2026-11-26 without the camera (the desktop held only a Sony A6000 in
charging mode, no UVC node): zune-jpeg 0.5 decodes 1280x720 JPEGs to within
0.05 mean per-channel difference of libjpeg-turbo and ffmpeg for 4:4:4,
4:2:2, 4:2:0, 4:1:1 and 4:4:0 sampling, progressive, non-interleaved
multi-scan, restart markers (by MCU count and by row), float DCT, optimized
Huffman tables, and RGB and gray colorspaces, and `from_mjpeg`'s 4:2:2 and
4:2:0 output looks identical to ffmpeg's decode. That
rules out stride, chroma and layout bugs for any ordinary JPEG, so the
squash comes from something specific to the camera's frames. A frame without
Huffman tables (legal in UVC MJPEG) errors loudly (`No DC table for
component Y`) instead of rendering wrongly, so it is not the squash either;
arithmetic coding errors loudly too. What stays open is the frame itself
(its markers, or a JPEG that is not 1280x720 as the SOF says) and the
PipeWire buffer that holds it, which only a captured frame separates. Land
no decode change before that frame is in hand.

## Hosts

- linux - the camera is on the maintainer's Fedora desktop

## Waiting

- Maintainer (PC): capture one real frame from the ZV-E10 and check both
  paths end to end, since no agent session can reach the camera. Plug it in
  as a webcam (not charging mode) and quit anything using it (the studio, a
  browser, OBS), then in the repo run `nix shell nixpkgs#v4l-utils -c v4l2-ctl --list-devices`
  and pick the ZV-E10's first `/dev/videoN` (the second is metadata).
  Confirm the format with `v4l2-ctl -d /dev/videoN --get-fmt-video`, then
  `v4l2-ctl -d /dev/videoN --set-fmt-video=width=1280,height=720,pixelformat=MJPG --stream-mmap --stream-count=3 --stream-to=.tmp/zv-e10.mjpeg`
  (all inside the same `nix shell`), and
  `ffmpeg -i .tmp/zv-e10.mjpeg -frames:v 1 -c copy .tmp/zv-e10.jpg` to split
  one frame; keep it under 500 KiB. Hand `.tmp/zv-e10.jpg` to an agent in this
  checkout to settle the cause and land the fix with the frame as the test
  fixture. Then run `moq devices`, publish the camera with
  `moq ... import capture --camera <id>` once as the `pipewire:` id and once
  as `/dev/videoN` (see doc/bin/cli.md), play each back, and check the picture
  is whole and untinted; the quest finishes when both are.
