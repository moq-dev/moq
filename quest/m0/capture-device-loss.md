# [M] moq-video: an AVFoundation camera that disappears parks the reader forever

## Goal

Unplugging a camera, revoking its permission, or losing the session to a
runtime error ends `capture::Stream::read` with a typed error instead of
leaving it parked.

## Plan

Every capture backend except one reports device loss. The pump-driven backends
(V4L2, Media Foundation, Desktop Duplication, X11, Windows GDI) surface a read
failure, which `pump::spawn` turns into `chan.fail`. ScreenCaptureKit has
`didStopWithError`, which does the same. AVFoundation has neither: after the
first frame the only callback pushes samples, and nothing watches for
`AVCaptureSessionRuntimeError`, `AVCaptureDeviceWasDisconnected`, or an
interruption. So a camera that vanishes stops delivering frames and nobody
closes the channel; `read` waits on a notification that never comes.

`capture::Stream::read` documents `Ok(None)` as a benign stop and an error as
terminal for that selection, and this path delivers neither. `capture_loop`
still races the read against demand, so the publisher releases the device when
the last viewer leaves; what it cannot do is notice that the camera went away
while someone is watching.

`SessionGuard` is the natural owner: it already ties the session's lifetime to
the stream, so it can hold the notification observers too. Translate device
removal and permission revocation into `chan.fail` with
`SourceUnavailable`/`PermissionDenied`, and decide explicitly what an
*interruption* means, since a phone call taking the camera is recoverable in a
way that unplugging it is not.

Pre-existing, not introduced by the native screen capture work; found by Codex
while reviewing [#3244](https://github.com/moq-dev/moq/pull/3244). Verifying it
means unplugging a real camera on a Mac, so an injectable termination hook is
what makes a regression test possible.
