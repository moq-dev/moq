# [M] MediaCodec decode on Android

## Goal

On Android, `moq-audio` decodes AAC-LC, HE-AAC v1 and v2, and multichannel
AAC through `AMediaCodec`, and whichever of MP3, FLAC, AC-3, and E-AC-3 the
device's codec list opens.

## Plan

The audio counterpart of `rs/moq-video/src/decode/backend/mediacodec.rs`,
behind the existing `mediacodec` feature and the decode seam, on `target_os
= "android"`.

- `audio/mp4a-latm` with the catalog description as `csd-0`; the output format
  reports the channel count and, on API 23 and later, the channel mask that
  maps to `Layout`.
- Optional codecs are probed through `AMediaCodecList` at open and advertised
  only where present.
- Fixtures and layout-order tests as in the AudioToolbox quest; runtime proof
  on a device or emulator, since no CI runs Android.
- The binding ships in the moq-ffi Android slice, which is how Kotlin and Dart
  reach it.

## Required

- [Decode seam](/quest/m2/audio-codecs/decode-backend.md) - the candidate order this backend joins
- [Layout](/quest/m2/audio-codecs/layout.md) - what a multichannel frame is delivered as

## Related

- [Android capture](/quest/m2/mobile/video-android.md) - the video MediaCodec family this sits beside
