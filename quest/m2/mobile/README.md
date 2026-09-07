# Mobile

## Goal

MoQ runs natively on iOS and Android with the media path the platform
expects: the app captures, encodes, decodes, and renders through the shipped
bindings, and the Rust core carries the rest. Today `moq-ffi` ships iOS and
Android slices with raw video publishing and audio in both directions, but no
video consumer, and capture exists on neither platform.

## Plan

Order follows #700's phases. The codec-only MVP first: a video consumer in
`moq-ffi` closes the one gap that blocks an app from playing a broadcast at
all. Then the ownership decision, because it decides whether the capture
backends below are built in Rust or left to Swift and Kotlin, and the
Android one is an XL bet. Capture and the Dart device proof follow.

## Quests

- [FFI video consumer](/quest/m2/mobile/ffi-video-consumer.md) - an app decodes a subscribed video rendition through moq-ffi on every binding that ships codecs
- [Ownership boundary](/quest/m2/mobile/ownership-boundary.md) - settle whether Rust or the platform owns capture, codecs, and rendering before growing either
- [iOS capture](/quest/m2/mobile/video-ios.md) - moq-video captures the camera and screen on iOS, reusing the VideoToolbox backend
- [Android capture](/quest/m2/mobile/video-android.md) - Camera2, MediaProjection and MediaCodec, a whole NDK/JNI backend family
- [Dart on iOS](/quest/m2/mobile/dart-ios.md) - prove the shipped iOS native asset actually loads on a device, which no CI can

## Closes

- [#700](https://github.com/moq-dev/moq/issues/700) - close this issue when the questline finishes

## Related

- [#933](/quest/m1/933-video-rotation-metadata-not-propagated-from-mobile-camera.md) - rotation metadata from a mobile camera
- [Video hardware validation](/quest/m3/video-hardware.md) - physical hardware evidence for each claimed GPU path
