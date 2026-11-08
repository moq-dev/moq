# [M] MediaCodec decode on Android

## Goal

On Android, `moq-audio` decodes AAC-LC, HE-AAC v1 and v2, and multichannel
AAC through `AMediaCodec`, and whichever of MP3, FLAC, AC-3, and E-AC-3 the
device's codec list opens.

## Plan

Rust owns capture and codecs on mobile, settled in the 2026-09-30 audit, so this backend is
the Android audio codec path.

The audio counterpart of `rs/moq-video/src/decode/backend/mediacodec.rs`,
behind a new optional audio `mediacodec` feature and the decode seam, on `target_os
= "android"`.

- `audio/mp4a-latm` with the catalog description as `csd-0`; the output format
  reports the channel count and, on API 23 and later, the channel mask that
  maps to `Layout`.
- Optional codecs are probed through `AMediaCodecList` at open and advertised
  only where present.
- Fixtures and layout-order tests as in the AudioToolbox quest; runtime proof
  on a device or emulator through `just rs android-test`, which android.yml's
  `emulator` job runs. Extend `sh/rs/android-test.sh` to run moq-audio's
  ignored MediaCodec tests beside moq-video's, and add the audio feature to the
  `just rs android` compile lane.
- The binding ships in the moq-ffi Android slice, which is how Kotlin and Dart
  reach it.

Changed 2026-11-07 (maintainer, /quest-plan from the API sweep of open PRs and remaining quests): [audio-codec-seam](/quest/m2/audio-codec-seam.md) makes moq-audio's codecs return zero or more outputs with `flush` before the cut, so this backend adds no break.

## Related

- [Android capture](/quest/m3/mobile-capture-android.md) - the video MediaCodec family this sits beside
