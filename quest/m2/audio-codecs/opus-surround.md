# [S] Opus surround through the multistream decoder

## Goal

An Opus track with channel mapping family 1 (up to 7.1, RFC 7845 §5.1.1)
decodes on every host, delivered in the canonical `Layout` order. Pure Rust,
so this is the one multichannel path Linux gets.

## Plan

- `moq_mux::codec::opus::Config` parses the channel mapping (family, stream
  count, coupled count, and the mapping table) instead of skipping it. Family
  0 stays mono/stereo; family 1 maps to a `Layout` by channel count; family
  255 and unknown families are refused, since they carry no speaker
  assignment.
- The Opus decode backend opens `opus_multistream_decoder_create` from
  `unsafe-libopus` when the family is 1, and reorders Vorbis order into the
  canonical one on the way out.
- Encode stays family 0; `Config::encode` keeps refusing more than two
  channels.
- Regression: a family-1 5.1 fixture decodes to six channels in canonical
  order; family 255 is refused at construction.

## Required

- [Layout](/quest/m2/audio-codecs/layout.md) - the type the mapping resolves to
