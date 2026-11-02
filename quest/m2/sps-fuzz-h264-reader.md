# [XS] Re-run the SPS fuzzer on h264-reader

## Goal

The H.264 SPS fuzz target runs against moq-mux's parser after [moq-dev/moq#7398](https://github.com/moq-dev/moq/pull/7398) moves it onto `h264-reader`. Any overflow or panic it finds there is fixed in moq-mux's validation and recorded in the fuzz-findings SPS quest.

## Plan

Decided 2026-10-30 (maintainer, follow-up planning): [moq-dev/moq#7313](https://github.com/moq-dev/moq/pull/7313) found the crop and width overflows in `h264-parser` ([therishidesai/h264-parser#9](https://github.com/therishidesai/h264-parser/issues/9)). #7398 replaces that crate for H.264, so the finding has to be rechecked on the new parser. Run the target long enough to cover the old reproducer and a fresh corpus. If `h264-reader` is clean, narrow the SPS quest to H.265.

Public API: none. Wire: none.

## Required

- [Media parsers have fuzz targets](/quest/m2/media-fuzz.md) - its PR, #7313, adds the SPS target and the fuzz-findings epic
