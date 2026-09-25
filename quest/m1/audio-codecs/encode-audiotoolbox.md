# [M] AudioToolbox AAC encode on macOS and iOS

## Goal

On macOS and iOS, `Codec::Aac` encodes through AudioToolbox at the input's
layout, up to 7.1, and the result plays in the browser, `moq play`, and OBS.

## Plan

An `AudioConverter` from interleaved `f32` to `kAudioFormatMPEG4AAC`, behind
the encode seam as the platform candidate on macOS and iOS.

- The catalog ASC is synthesized at construction per the encode seam; read
  `kAudioConverterCompressionMagicCookie` at open and assert it matches.
  `kAudioConverterPrimeInfo` gives the delay the timestamps fold in.
- Bitrate through `kAudioConverterEncodeBitRate`, updated live where the
  converter allows.
- The seam assumes one packet per frame. If the converter holds output back,
  the backend needs a `flush` and a zero-or-more return, which changes
  `Encoder::encode` and so targets `dev`.
- Gate the seam's "AAC refused without a platform encoder" test to hosts
  without one.
- Regression: a stereo and a 5.1 encode round-trip through the AudioToolbox
  decoder and through symphonia (stereo only), with timestamps continuous
  across the priming.

## Required

- [AudioToolbox decode](/quest/m1/audio-codecs/decode-audiotoolbox.md) - the round-trip regression decodes through it
