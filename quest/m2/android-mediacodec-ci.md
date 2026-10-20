# [M] MediaCodec video runs on an Android emulator

## Goal

moq-video's ignored MediaCodec tests,
`encodes_a_keyframe_with_parameter_sets_inline` and
`decodes_what_the_encoder_produced`, run nightly on an Android emulator
instead of never.

## Plan

`android.yml` only compiles the Android backends (`just rs android`); no job
runs a test on Android. The emulator's Codec2 software H.264 codecs
(`c2.android.avc.*`) are enough for both tests, whose ignore reasons ask for
hardware.

- First check whether the MediaCodec backend refuses software codecs when it
  picks one. If it does, the emulator needs a way to allow them in tests;
  otherwise correct the ignore reasons to "an Android device or emulator".
- Run the cross-compiled test binary on an x86_64 emulator with KVM on a
  hosted Linux runner (`reactivecircus/android-emulator-runner`, pinned by
  commit) through `adb push` and `adb shell`, behind one recipe, so a local
  emulator or device runs the same thing.
- The [MediaCodec decode](/quest/m2/audio-decode-mediacodec.md) and
  [MediaCodec encode](/quest/m2/audio-encode-mediacodec.md) audio quests
  need the same lane; land it here and let them reuse it.

Public API: none. Wire: none.

## Related

- [Android capture](/quest/m2/mobile-capture-android.md) - device capture, which an emulator cannot prove
