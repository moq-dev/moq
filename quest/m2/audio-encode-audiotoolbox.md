# [M] AudioToolbox AAC encode on macOS and iOS

## Goal

On macOS and iOS, `Codec::Aac` encodes through AudioToolbox at the input's
layout, mono through 5.1 and 7.1, and the result plays in the browser,
`moq play`, and OBS. 6.1 is refused: since #4973 a synthesized ASC names no
program config element. Carrying the converter's magic cookie verbatim would
lift that once a caller needs it.

## Plan

An `AudioConverter` from interleaved `f32` to `kAudioFormatMPEG4AAC`, behind
the encode seam as the platform candidate on macOS and iOS.

- The catalog ASC is synthesized at construction per the encode seam; read
  `kAudioConverterCompressionMagicCookie` at open and assert it matches.
  `kAudioConverterPrimeInfo` gives the delay the timestamps fold in.
- Bitrate through `kAudioConverterEncodeBitRate`, updated live where the
  converter allows.
- This quest owns changing `Encoder::encode` from one packet per frame to
  a zero-or-more packet return with a flush operation. MediaCodec requires
  this shared contract even if AudioToolbox itself emits immediately. Prove
  delayed and multiple outputs and flushing in the shared encoder tests.
- Gate the seam's "AAC refused without a platform encoder" test to hosts
  without one.
- Regression: a stereo and a 5.1 encode round-trip through the AudioToolbox
  decoder and through symphonia (stereo only), with timestamps continuous
  across the priming.
- That continuity fails today for an epoch below the delay: `Producer::timestamp`
  (`rs/moq-audio/src/encode/producer.rs`) clamps each priming packet to zero,
  so a zero epoch stamps `[0, 0, 0, 20_000]`, and `decode::Consumer` reads the
  duplicates as holes and resets AAC between them. Shift the whole timeline by
  the deficit instead, including the terminal end (found in #4081 review).
- Until [FFI codec namespaces](/quest/m1/ffi-shape/codec.md) fixes the
  frame duration default, binding callers pass `frame_duration_us: 0` with
  `aac()`.

## Required

- [AudioToolbox decode](/quest/m2/audio-decode-audiotoolbox.md) - the round-trip regression decodes through it

## Related

- [MediaCodec AAC encode](/quest/m2/audio-encode-mediacodec.md) - the same encode seam on Android
- [Media Foundation AAC encode](/quest/m2/audio-encode-mediafoundation.md) - the same encode seam on Windows
