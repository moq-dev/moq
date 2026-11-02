# [XS] moqsrc plays surround Opus, under test

## Goal

A test proves that a 3 to 8 channel Opus rendition published through
`moqsink` plays back through `moqsrc` and `opusdec` with every channel.

## Plan

Decided 2026-10-22 (maintainer, from the release audit): m2. Receive never
worked, so this is not a regression.
[moq-dev/moq#4991](https://github.com/moq-dev/moq/pull/4991) fixed publishing
and named this follow-up.

The caps side landed in
[moq-dev/moq#6284](https://github.com/moq-dev/moq/pull/6284): `opus_caps` in
`rs/moq-gst/src/source/imp.rs` parses the catalog's OpusHead with
`opus::Mapping`, sets `channel-mapping-family`, `stream-count`,
`coupled-count`, and `channel-mapping`, and sends the OpusHead plus an empty
OpusTags as the `streamheader`, since `opusdec` reads the head (and its output
gain) only from a two-buffer `streamheader`. Its unit test,
`opus_caps_carry_the_stream_layout`, covers only mono and stereo.

What remains is the end-to-end test: `opusenc` 5.1 through `moqsink`, a
relay, `moqsrc`, and `opusdec`, checking six decoded channels, plus each
channel count from 3 to 8 in the caps unit test, since every one maps
through the same family-1 table.

Public API: none. Wire: none.
