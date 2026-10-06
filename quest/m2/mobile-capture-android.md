# [XL] Android capture and encode

## Goal

`moq-video` captures and encodes on Android: Camera2 or CameraX for the
camera, MediaProjection for the screen, and MediaCodec for encode and decode.

## Plan

Rust owns capture and codecs on mobile, settled in the 2026-09-30 audit. MediaCodec encode/decode already exist in moq-video. Reuse them rather than
planning a second backend family. The remaining capture and native Surface
integration needs NDK/JNI lifecycle, synchronization, and actual device proof.

This is the Android SDK's media path, not a Rust-only extra. `moq-kit`'s
Kotlin capture is the parallel stack the verdict retires once this lands.
Frames cross `moq-ffi` as opaque `HardwareBuffer`/`Surface` handles rather than
copies. Rust-native consumers benefit too (the same gap that made `iroh-live`
reimplement the native layer).

`moq-tokio` already reaches into Android through JNI for `tls::init_android`,
so the mechanism exists.

## Related

- [iOS capture](/quest/m2/mobile-capture-ios.md) - the other half of mobile, which
  reuses an existing backend rather than adding one
