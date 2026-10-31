# [S] SPS fields cannot overflow

## Goal

A hostile H.264 or H.265 SPS reaching moq-mux (fMP4 `avcC`/`hvcC`, Annex-B,
or MPEG-TS) is refused or ignored without integer overflow, so debug builds
do not panic and release builds do not wrap into wrong dimensions. The
`fmp4`, `annexb`, and `ts` targets' SPS inputs are committed under
`rs/moq-mux/fuzz/regressions/` and pass.

## Plan

The fuzzers found:

- h264-parser 0.4.2 `sps.rs:186,197,204`: `(pic_width_in_mbs_minus1 + 1) * 16`
  and the crop subtraction overflow on large exp-Golomb values.
- scuffle-h265 0.2.2 `sps/mod.rs:303,305` (`ctb_log2_size_y - min_tb_log2_size_y`
  in a `range_check!`) and `sps/scaling_list.rs:90`
  (`matrix_id - scaling_list_pred_matrix_id_delta`): subtract overflow. The
  scaling-list one also panics in release builds, on the wrapped index at
  `:92`.
- scuffle-expgolomb 0.1.5 `read_exp_golomb`: 64 or more leading zeros
  underflow `result - 1` (release returns `u64::MAX`).

Validate the SPS fields in moq-mux before calling the parser, refusing an
SPS that would reach those lines (decided 2026-10-30, maintainer). moq-mux
already reads SPS bits itself with `h264_parser::bitreader` (the H.265 head
in `codec/h265/sps.rs`, and `codec/annexb.rs`). SPS parsing stays on the
crates moq-mux uses now, h264-parser for H.264 and scuffle-h265 plus
`codec/h265/sps.rs` for H.265; it does not move to mp4-atom, which stays
box-level.

Reported upstream, without waiting on a fix:
[therishidesai/h264-parser#9](https://github.com/therishidesai/h264-parser/issues/9)
(filed 2026-10-25 by another moq-mux user, covering the crop and width
overflows), [ScuffleCloud/scuffle#653](https://github.com/ScuffleCloud/scuffle/issues/653)
(scuffle-h265), and [ScuffleCloud/scuffle#654](https://github.com/ScuffleCloud/scuffle/issues/654)
(scuffle-expgolomb). Each has a minimal reproducer. scuffle-h265 was last
released in 2025-05.

Public API: none. Wire: none.
