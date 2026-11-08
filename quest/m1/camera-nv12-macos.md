# [S] A macOS camera delivers frames the renderer imports

## Goal

A camera opened through `moq_video::capture` on macOS delivers NV12 pixel
buffers, which the Metal import and the CPU download both read, instead of
the device's native format. Today a MacBook Air camera delivers `2vuy`, and
`Output::Render` fails at the first frame with `cannot download pixel format
0x32767579; expected NV12`.

## Plan

Found by moq-dev/studio's first Mac capture run
([moq-dev/studio#70](https://github.com/moq-dev/studio/pull/70)). Decided
2026-11-07 (maintainer, moq-dev/studio's /quest-unblock): ✅ m1 (rejected: m3),
since it is an additive fix to shipped code and blocks the studio's macOS
camera.

`open` in `rs/moq-video/src/capture/avfoundation.rs` builds its
`AVCaptureVideoDataOutput` without `videoSettings`, so AVFoundation passes the
device's format through. Request `kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange`
through `kCVPixelBufferPixelFormatTypeKey`, as the display source already does
(`rs/moq-video/src/capture/screencapture.rs`). A local patch doing this
brought the camera to the studio's program and broadcast (1280x720 and
640x360 H.264 at 30 fps, correct colors).

CI has no camera: test that the output requests NV12 by reading its
`videoSettings` back, and record a run on a Mac with a camera in the PR.

Once it lands, moq-dev/studio bumps its `moq` pin.
