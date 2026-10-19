# [XS] The access unit delimiter stays first

## Goal

When a keyframe's payload starts with an access unit delimiter, every Annex B
writer emits the AUD, then the re-injected parameter sets, then the rest, as
H.264 §7.4.1.2.3 and H.265 §7.4.2.4.4 require. Today `export ts` writes SPS,
PPS, AUD, IDR.

## Plan

Decided 2026-10-19 by the maintainer (#5951):

- Fix both writers: `video_es_payload` in
  `rs/moq-mux/src/container/ts/export.rs` and `annexb::from_length_prefixed`
  in `rs/moq-mux/src/codec/annexb.rs`. The second also feeds the H.264 and
  H.265 elementary exporters, `moq-rtc`, and the `moq-video` decoder, which
  all get the right order.
- An AUD is H.264 NAL type 9 or H.265 type 35. A payload without one is
  unchanged.
- Regression test: an AUD + IDR keyframe exports as AUD, SPS, PPS, IDR. It
  fails today.

Public API: none.

## Closes

- [#5951](https://github.com/moq-dev/moq/issues/5951) - export ts writes keyframe parameter sets ahead of the AUD
