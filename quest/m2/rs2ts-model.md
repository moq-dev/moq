# [XL] Generated model

## Goal

@moq/net's model layer and kio are generated from moq-net by rs2ts, and the
hand-written TypeScript they replace is deleted. The translated model tests
and `just test interop --all` pass. This is the risky stage of the
[rs2ts line](/quest/m1/rs2ts/README.md); the
[sans-IO lite session](/quest/m2/rs2ts-sans-io-lite.md) waits on it.

## Plan

Decided 2026-10-11: the model-stage moq-net refactors are listed here and
split into their own quests when this stage starts, after the codec
go/no-go:

- Rewrite the `unsafe` frame and group storage (`model/frame.rs` raw-pointer
  segments, `model/group.rs` `[MaybeUninit<Frame>; 8]`) in safe Rust.
  Decided over an extern TS binding, for one implementation. Benchmark the
  native hot path with `--bench` before and after, and report any
  regression for the maintainer to judge.
- Rewrite `run_front`/`serve_front` (`model/origin.rs`) as a named poll
  machine, since rs2ts does not translate async.
- Name the nested `Option` states in `model/resume.rs` with enums.
- Remove the model's u128 and FNV math at the source, or rely on
  [model support](/quest/m2/rs2ts-model-support.md) for them.
- Drop the `&mut` scalar out-params (`model/origin.rs:2480`).

API, carried from the original plan:

- The `@moq/net` API may change where the generated shape is no worse to
  use: disposable handles (`using`), `U64` for sequences and ids. Update
  watch, publish, hang, room, and the demos in the same change, and the
  `doc/` pages for anything user-facing.
- A forgotten `drop()` leaves a track open forever: add a debug-only
  `FinalizationRegistry` that reports handles collected without one, and
  runtime guards against double drop and use after drop.

Public API: breaks `@moq/net`. Wire: none.

## Required

- [Generated codec](/quest/m1/rs2ts/codec.md) - the pipeline and the go/no-go
- [Model support](/quest/m2/rs2ts-model-support.md) - the translator handles kio and the model's constructs
- [The async feature](/quest/m1/net-async-feature.md) - rs2ts reads moq-net without it
