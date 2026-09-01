# [L] iOS capture

## Goal

`moq-video` captures on iOS: the camera through AVFoundation and the screen
through ReplayKit.

## Plan

Not a new codec backend. VideoToolbox already encodes and decodes as the macOS
backend and works on iOS unchanged, and `Surface::PixelBuffer` already carries
a `CVPixelBuffer` zero-copy, so this is capture wiring plus the lifecycle iOS
imposes and macOS does not.

That lifecycle is the work. Camera and screen access are permission-gated and
revocable, an app is suspended and resumed on foreground changes, and
ReplayKit's broadcast extension runs in a separate process with a hard memory
cap. Capture has to open on demand, survive being interrupted, and release the
device when it stops, rather than assuming a session it opened stays valid.

Reuse the `capture::Source` shape the other platforms use rather than growing
an iOS-specific entry point, so device enumeration and selection behave the
same everywhere.

## Related

- [Android capture and encode](/quest/m2/video-android.md) - the other half of
  mobile, and a much larger one
