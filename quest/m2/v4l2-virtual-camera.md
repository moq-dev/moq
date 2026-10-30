# [S] V4L2 MJPEG capture runs against a virtual camera

## Goal

moq-video's V4L2 capture path streams MJPEG nightly from a virtual camera, so
negotiating MJPEG and decoding only each buffer's `bytesused` are tested
without a camera.

## Plan

Nightly's `rs v4l2` lane streams YUYV from the kernel's `vivid` driver, and a
fixture test covers `I420::from_mjpeg`. `vivid` offers raw formats only, so
nothing streams MJPEG through a V4L2 device yet.

- `v4l2loopback` fed by FFmpeg can offer MJPEG. It is not in Ubuntu's kernel
  packages, so the nightly job builds it through `v4l2loopback-dkms`, which
  needs `linux-headers-$(uname -r)`.
- Open question: whether this is worth a DKMS build in nightly, given the
  MJPEG-only code left untested is negotiating the format and slicing the
  buffer to `bytesused`.
- Extend the `vivid` tests in `rs/moq-video/src/capture/v4l2.rs`: find the
  loopback node by driver name, stream a few MJPEG frames, assert their size
  and count, and skip unless `CI_STRICT`.

Public API: none. Wire: none.

## Related

- [Video hardware validation](/quest/m3/video-hardware.md) - a live camera per platform, which a virtual one cannot stand in for
- [Linux capture parity](/quest/m2/capture-linux.md) - the other Linux capture paths
