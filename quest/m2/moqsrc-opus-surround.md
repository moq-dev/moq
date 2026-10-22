# [S] moqsrc plays surround Opus

## Goal

`moqsrc` outputs `audio/x-opus` caps that `opusdec` accepts for a 3 to 8
channel Opus rendition, so surround Opus published through `moqsink` plays
back through GStreamer.

## Plan

Decided 2026-10-22 (maintainer, from the release audit): m2. Receive never
worked, so this is not a regression.
[moq-dev/moq#4991](https://github.com/moq-dev/moq/pull/4991) fixed publishing
and named this follow-up.

`rs/moq-gst/src/source/imp.rs` builds the caps from rate and channels, plus
`codec_data` and `stream-format=ogg` when a description exists. `opusdec`
ignores `codec_data` and reads `streamheader` or the mapping fields, so with
more than two channels it assumes family 0 and refuses.

Parse the catalog's OpusHead description with the existing `opus::Mapping`
and set `channel-mapping-family`, `stream-count`, `coupled-count`, and
`channel-mapping`. This needs no synthesized OpusTags, unlike a
`streamheader`. Test `opusenc` 5.1 through `moqsink`, a relay, `moqsrc`, and
`opusdec`, checking six decoded channels. The only moqsrc test rendition
today is stereo.

Public API: none. Wire: none.

## Related

- [CMAF surround Opus](/quest/m2/cmaf-opus-surround.md) - the same mapping table through fMP4 `dOps`, independent of this caps path
