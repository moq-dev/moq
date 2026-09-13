# [M] An extensible audio codec object in the bindings

## Goal

Binding callers select an audio codec through an immutable typed object rather
than an exhaustive enum. Opus works before release; adding an AAC constructor
later does not break callers switching over a closed generated enum.

## Plan

Replace the closed `MoqAudioCodec` UniFFI enum with an immutable object privately
holding the native encode codec. Expose a named Opus constructor, presented as
`AudioCodec.opus()` where the language supports that spelling. Encoder output
configuration accepts that object. Preserve the existing Opus default and
actual Opus encode behavior. Do not expose arbitrary strings or numeric tags,
a placeholder AAC variant, or another codec implementation.

Update generated bindings and every hand-written wrapper in Python, Swift,
Kotlin, Go, and Dart, plus their docs and examples. Verify constructor naming
and passing an object inside configuration with each generator. Keep the
native `moq-audio` codec implementation private behind the binding object;
its existing non-exhaustive enum does not need this representation change.
The C API already selects codecs by string and needs no enum migration here.

Use packaged consumer tests in CI to construct the Opus selection, place it
in configuration, encode actual audio, and release the codec/configuration in
different orders. The object is immutable and its lifetime is retained by the
configuration that needs it. Add public accessors only for an actual consumer.

This is a published binding API break and lands on dev. There is no wire or
catalog change. M2 adds the AAC constructor with its encoder implementation;
no AAC capability is promised by this groundwork.

## Related

- [AAC encode seam](/quest/m2/audio-codecs/encode-backend.md) - adds AAC selection through a constructor
- [External API proof](/quest/m1/api-release-proof.md) - validates generated and packaged callers before release
