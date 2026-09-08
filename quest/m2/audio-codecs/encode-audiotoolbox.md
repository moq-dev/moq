# [M] AudioToolbox AAC encode on macOS and iOS

## Goal

On macOS and iOS, `Codec::Aac` encodes through AudioToolbox at the input's
layout, up to 7.1, and the result plays in the browser, `moq play`, and OBS.

## Plan

An `AudioConverter` from interleaved `f32` to `kAudioFormatMPEG4AAC`, behind
the encode seam as the platform candidate on macOS and iOS.

- Read the magic cookie after the first packet for the catalog description,
  and `kAudioConverterPrimeInfo` for the delay the timestamps fold in.
- Bitrate through `kAudioConverterEncodeBitRate`, updated live where the
  converter allows.
- Regression: a stereo and a 5.1 encode round-trip through the AudioToolbox
  decoder and through symphonia (stereo only), with timestamps continuous
  across the priming.

## Required

- [Encode seam](/quest/m2/audio-codecs/encode-backend.md) - the candidate order this backend joins
- [Layout](/quest/m2/audio-codecs/layout.md) - the input layout the encoder accepts
