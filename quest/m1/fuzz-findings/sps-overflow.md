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
- scuffle-h265 0.2.2 `sps/mod.rs:303` (`ctb_log2_size_y - min_tb_log2_size_y`
  in a `range_check!`), `sps/scaling_list.rs`, and scuffle-expgolomb 0.1.5:
  subtract overflow.

All three crates are outside the orgs this repository may post to. Open, for
the maintainer before building, with a recommendation: validate the SPS
fields in moq-mux before calling the parser (it already reads some SPS bits
itself in `codec/annexb.rs`), report or fix upstream once approved, or fork
or replace the parser. scuffle-h265 was last released in 2025-05.

Public API: none. Wire: none.
