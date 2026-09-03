# [XL] Android capture and encode

## Goal

`moq-video` captures and encodes on Android: Camera2 or CameraX for the
camera, MediaProjection for the screen, and MediaCodec for encode and decode.

## Plan

A whole backend family, not a port. Every other platform backend is objc2 or a
C API; this one is NDK and JNI against the Android framework, and MediaCodec's
Surface-in/Surface-out model is natively zero-copy in a shape none of the
existing backends share.

Weigh the cost honestly before starting. `moq-kit` already does this in Kotlin
over `moq-ffi`, and raw frames cannot cross the FFI boundary zero-copy, so a
Rust Android media path pays off for Rust-native consumers and not for the
mobile SDK. That is a real audience (this is the same gap that made
`iroh-live` reimplement the native layer) but it is worth naming, since it
decides whether XL is worth spending.

`moq-tokio` already reaches into Android through JNI for `tls::init_android`,
so the mechanism exists.

## Related

- [iOS capture](/quest/m2/video-ios.md) - the other half of mobile, which
  reuses an existing backend rather than adding one
