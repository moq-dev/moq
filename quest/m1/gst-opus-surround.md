# [S] Surround Opus from GStreamer

## Goal

`moq-gst`'s sink publishes a 3 to 8 channel `audio/x-opus` stream with the
OpusHead its caps describe, so surround Opus from `opusenc` plays natively like
the same stream from Matroska or MPEG-TS.

## Plan

- The sink refuses more than two channels because it builds the head from
  `channels` and `rate` alone. Multichannel Opus caps also carry
  `channel-mapping-family`, `stream-count`, `coupled-count`, and
  `channel-mapping`; build the mapping with `opus::Mapping::new`, which
  validates them, and refuse caps that omit or contradict them.
- Prefer a `streamheader` OpusHead when the caps carry one, if that path is
  simpler and covers the same streams.
- Regression: an `opusenc` 5.1 pipeline publishes a family 1 description that
  `moq-audio` decodes to six channels.

Public API: none. Wire: none.
