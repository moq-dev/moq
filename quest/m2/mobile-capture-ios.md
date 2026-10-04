# [L] iOS capture

## Goal

`moq-video` captures on iOS: the camera through AVFoundation and the screen
through ReplayKit.

## Plan

Rust owns capture and codecs on mobile, settled in the 2026-09-30 audit ([Mobile ownership](/quest/m1/mobile-ownership.md)).
Not a new codec backend. VideoToolbox encode, decode, and the native
PixelBuffer surface compile on iOS (the `apple` cfg in moq-video). Verify the
runtime path on a device rather than assuming desktop behavior. The new work is
capture wiring plus the lifecycle iOS imposes and macOS does not.

That lifecycle is the work. Camera and screen access are permission-gated and
revocable, an app is suspended and resumed on foreground changes, and
ReplayKit's broadcast extension runs in a separate process with a hard memory
cap. Capture has to open on demand, survive being interrupted, and release the
device when it stops, rather than assuming a session it opened stays valid.

Reuse the `capture::Source` shape the other platforms use rather than growing
an iOS-specific entry point, so device enumeration and selection behave the
same everywhere.

## Related

- [Android capture and encode](/quest/m2/mobile-capture-android.md) - the other half of
  mobile, and a much larger one
