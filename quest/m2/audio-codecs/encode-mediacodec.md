# [M] MediaCodec AAC encode on Android

## Goal

On Android, `Codec::Aac` encodes through `AMediaCodec` at the input's layout
where the device's encoder supports it.

## Plan

The audio counterpart of `rs/moq-video/src/encode/backend/mediacodec.rs`,
behind the `mediacodec` feature and the encode seam.

- `audio/mp4a-latm` with `KEY_AAC_PROFILE` = LC. The catalog ASC is
  synthesized at construction per the encode seam, since `csd-0` only arrives
  with the first output buffer; assert the two match.
- Multichannel is device-dependent; probe the encoder's capabilities at open
  and refuse a layout it does not list.
- Round-trip regression through the MediaCodec decoder; runtime proof on a
  device or emulator.

## Required

- [Encode seam](/quest/m2/audio-codecs/encode-backend.md) - the candidate order this backend joins
- [Layout](/quest/m2/audio-codecs/layout.md) - the input layout the encoder accepts
- [MediaCodec decode](/quest/m2/audio-codecs/decode-mediacodec.md) - the round-trip regression decodes through it
