# [S] Surround Opus from MPEG-TS

## Goal

An MPEG-TS Opus stream with 3 to 8 channels imports with an OpusHead that
decodes, so it plays natively like the same stream from Matroska or FLV.

## Plan

- The TS importer knows only the `channel_config_code` from the Opus
  extension descriptor and builds `opus::Config::new(48_000, channels)`, whose
  `encode` refuses more than two channels. The catalog then carries no
  description and `moq-audio` refuses the track.
- Codes 3 to 8 mean family 1 with the Vorbis default stream and coupled counts
  and mapping (the Vorbis orders of RFC 7845 §5.1.1.2), so the
  importer can synthesize the head. That needs a way to build a family 1
  `Config` and have `encode` emit its table; decide whether that is a
  constructor on `opus::Mapping` or a TS-local head writer.
- Regression: a 5.1 TS fixture imports with a family 1 description and decodes
  to six channels.
