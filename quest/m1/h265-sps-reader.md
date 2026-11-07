# [S] moq-mux reads H.265 SPS without scuffle

## Goal

moq-mux's `codec::h265` module parses the whole SPS with its own bounded
reader and returns the cropped size, VUI colour signal, and aspect ratio that
moq-mux and moq-video read today. scuffle-h265 and scuffle-expgolomb leave
the dependency tree. The committed `fmp4`, `annexb`, and `ts` fuzz
regressions and moq-video's SPS regression test still pass.

## Plan

Decided 2026-11-07 (maintainer, /quest-unblock): ✅ drop scuffle-h265 and
make the parser a module in moq-mux. ScuffleCloud/scuffle looks unmaintained
(last commit to main 2025-10-31; scuffle-h265 0.2.2 and scuffle-expgolomb
0.1.5 are from mid-2025; outside PRs such as
[ScuffleCloud/scuffle#652](https://github.com/ScuffleCloud/scuffle/pull/652)
sit unreviewed), and
[ScuffleCloud/scuffle#653](https://github.com/ScuffleCloud/scuffle/issues/653)
and [#654](https://github.com/ScuffleCloud/scuffle/issues/654) have no reply.
Not chosen: fork scuffle under moq-dev and publish patched crates; keep
waiting on upstream releases. Milestone: ✅ m1 (not chosen: m2, m4 as before).

moq-mux already walks every SPS field itself before handing it to scuffle
(`sps::check` and `walk` in `rs/moq-mux/src/codec/h265/sps.rs`, added by
[moq-dev/moq#7685](https://github.com/moq-dev/moq/pull/7685)), so turn that
walk into the parser:

- Read the fields the callers use instead of only bounding them, in the
  spec's layout (ITU-T H.265 7.3.2.2.1). Drop the places where the walk copies
  scuffle 0.2.2's non-spec layout, such as `sub_layer_level_idc` read twice
  and `iv_di_mc_enabled_flag` read once.
- Keep the head fallback for SPSs whose later fields break a semantic
  constraint real encoders violate (`matrix_coeffs` 0 on 4:2:0), and keep
  refusing truncated or out-of-range values.
- Replace the scuffle `NALUnitType`, `Profile`, `AspectRatioInfo`, and
  `VideoSignalType` uses in moq-mux (`h265/`, `container/ts/video_timing.rs`,
  `container/mkv/hvcc.rs`) and moq-video (`decode/vui.rs`) with moq-mux's own
  types, crate-private where moq-video doesn't need them.
- Keep every regression input, including the `scuffle-h265-*` and
  `scuffle-expgolomb` ones moq-video's tests include.

scuffle-av1 stays: it is the same upstream, but no bug in it is known.

Public API: none; moq-mux exports no scuffle type.
Wire: none.

## Related

- [Media fuzz findings](/quest/m1/fuzz-findings/README.md) - found the scuffle overflows
- [Re-run the SPS fuzzer on h264-reader](/quest/m4/sps-fuzz-h264-reader.md) - the H.264 half
