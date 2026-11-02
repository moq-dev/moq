# [M] Parse H.264 and codec config bits with maintained crates

## Goal

moq-mux parses H.264 parameter sets with `h264-reader`, which moq-video and
moq-mux's TS video timing (`container/ts/video_timing.rs`) already use, instead of `h264-parser` plus a hand-written VUI and HRD walker,
and its codec config readers share one maintained bit reader. Catalog output
is unchanged.

## Plan

Found by the 2026-10-19 dependency audit. `h264-parser` stops at
`vui_parameters_present_flag`, so `rs/moq-mux/src/codec/h264/mod.rs` re-reads
the SPS bit by bit (about 230 lines of `Vui`, `sps_vui`, and `read_hrd`).
`h264-reader` 0.9 exposes the VUI, HRD, and bitstream restriction, and
`rs/moq-video/src/decode/vui.rs` duplicates moq-mux's `sps_color` on top of it.
The H.265 SPS fallback in `codec/h265/sps.rs` borrows `h264-parser`'s reader.

Check first whether `h264-reader` covers the SEI recovery point and mastering
display parsing moq-mux does (`split.rs`, `color.rs`); keep those small
pieces hand-written if not. [Intra refresh import](/quest/m2/intra-refresh/h264-import.md)
reads `recovery_frame_cnt` through `h264-parser` today, so whichever lands
second adapts.

Four MSB-first bit readers repeat the same job for codec config: VP9
(`codec/vp9/mod.rs`), AAC (`codec/aac/mod.rs`, with a recording writer), the
TS Opus channel config (`container/ts/import.rs`), and moq-audio's own
AudioSpecificConfig scan (`rs/moq-audio/src/aac.rs`), which could call
moq-mux's parser instead. `bitstream-io` is already in the lock through
`h264-reader`. These are cold paths, so pick by API fit, not speed.

The existing codec tests and catalog fixtures are the regression gate. No
public API or wire change.
