# [XS] Re-run the SPS fuzzer on h264-reader

## Goal

The H.264 SPS fuzz target runs against moq-mux's parser after [moq-dev/moq#7398](https://github.com/moq-dev/moq/pull/7398) moves it onto `h264-reader`. Any overflow or panic it finds there is fixed in moq-mux, with its input committed under `rs/moq-mux/fuzz/regressions/`.

## Plan

Decided 2026-10-30 (maintainer, follow-up planning): [moq-dev/moq#7313](https://github.com/moq-dev/moq/pull/7313) found the crop and width overflows in `h264-parser` ([therishidesai/h264-parser#9](https://github.com/therishidesai/h264-parser/issues/9)). #7398 replaces that crate for H.264, so the finding has to be rechecked on the new parser. Run the target long enough to cover the old reproducer and a fresh corpus.

The H.265 half landed separately: moq-mux walks every H.265 SPS before scuffle_h265 parses it. For H.264, h264-reader 0.9 bounds `ue(v)` at 2^32 - 2 and computes the picture size with checked arithmetic, and the old reproducer is committed as `rs/moq-mux/fuzz/regressions/fmp4/h264-sps-width` and passes. That review was by reading the code, so this run is what confirms it.

Public API: none. Wire: none.
