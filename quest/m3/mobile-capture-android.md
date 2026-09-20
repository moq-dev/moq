# [XL] Android capture and encode

## Goal

`moq-video` captures and encodes on Android: Camera2 or CameraX for the
camera, MediaProjection for the screen, and MediaCodec for encode and decode.

## Plan

MediaCodec encode/decode already exist in moq-video. Reuse them rather than
planning a second backend family. The remaining capture and native Surface
integration needs NDK/JNI lifecycle, synchronization, and actual device proof.

Weigh the cost honestly before starting. `moq-kit` already does this in Kotlin
over `moq-ffi`, and raw frames cannot cross the FFI boundary zero-copy, so a
Rust Android media path pays off for Rust-native consumers and not for the
mobile SDK. That is a real audience (this is the same gap that made
`iroh-live` reimplement the native layer) but it is worth naming, since it
decides whether XL is worth spending.

`moq-tokio` already reaches into Android through JNI for `tls::init_android`,
so the mechanism exists.

## Required

- [Video timing](/quest/m0/video-timing.md) - timestamped capture and rational rates

- [Ownership boundary](/quest/m3/mobile-ownership.md) - decides whether an NDK/JNI backend family is worth building

## Related

- [iOS capture](/quest/m3/mobile-capture-ios.md) - the other half of mobile, which
  reuses an existing backend rather than adding one
