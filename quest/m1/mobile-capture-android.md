# [XL] Android capture

## Goal

`moq-video` captures on Android: Camera2 or CameraX for the camera and
MediaProjection for the screen, feeding the existing MediaCodec encoder.

## Plan

Rust owns capture and codecs on mobile, settled in the 2026-09-30 audit. MediaCodec encode/decode already exist in moq-video. Reuse them rather than
planning a second backend family. The remaining capture and native Surface
integration needs NDK/JNI lifecycle, synchronization, and actual device proof.

This is the Android SDK's media path, not a Rust-only extra. Frames cross `moq-ffi` as opaque `HardwareBuffer`/`Surface` handles rather than
copies. Rust-native consumers benefit too (the same gap that made `iroh-live`
reimplement the native layer).

`moq-tokio` already reaches into Android through JNI for `tls::init_android`,
so the mechanism exists.

Capture sets the catalog `rotation` from the sensor and display orientation,
so a phone held in portrait plays upright; the browser half of that is
[#933](/quest/m1/933-video-rotation-metadata-not-propagated-from-mobile-camera.md).

Promoted from m2 on 2026-10-09 by maintainer priority.

## Related

- [#933](/quest/m1/933-video-rotation-metadata-not-propagated-from-mobile-camera.md) - the same catalog rotation for browser capture
- [iOS capture](/quest/m1/mobile-capture-ios.md) - the other half of mobile, which
  reuses an existing backend rather than adding one
