# [M] V4L2 capture runs against a virtual camera

## Goal

`moq-v4l` and moq-video's V4L2 capture path run nightly against the kernel's
`vivid` virtual capture driver, so opening a device, negotiating a format,
MMAP streaming, and closing are tested without a camera.

## Plan

The m0 coverage audit measured `moq-v4l` at 9 of 1457 lines (0.6%); nothing
opens a device, and `rs/moq-video/src/capture/v4l2.rs` runs only on a
developer's camera.

- `vivid` ships in Ubuntu's `linux-modules-extra` package for the running
  kernel. GitHub-hosted runners grant `sudo`, so a nightly job can install
  that package and `modprobe vivid`. Check first that the hosted kernel's
  package carries it; if not, fall back to `v4l2loopback` fed by FFmpeg,
  which also needs the kernel headers to build.
- Add ignored tests in moq-video that open the `vivid` node by its driver
  name rather than a fixed `/dev/videoN`: enumerate formats and frame sizes,
  stream a few frames in a raw format and in MJPEG, assert their size and
  count, then close and reopen. Select them with a `just rs v4l2` recipe.
- A missing device fails under `CI_STRICT`, the convention the io_uring and
  mDNS tests use, and skips otherwise.
- Nightly only: loading a kernel module is a runner mutation no pull request
  job should make.

Public API: none. Wire: none.

## Related

- [Video hardware validation](/quest/m3/video-hardware.md) - a live camera per platform, which a virtual one cannot stand in for
- [Linux capture parity](/quest/m2/capture-linux.md) - the other Linux capture paths
