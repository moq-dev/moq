# [S] TS step scan stays ordered

## Goal

The MPEG-TS step detector's look-ahead never reads with `scan < at`, so its
`self.scan - self.at` staging check (`rs/moq-mux/src/container/ts/step.rs`,
`Ahead::scan`) neither overflows in debug nor wraps in release, which cut
the look-ahead short. The `ts` target's 567-byte input is committed under
`rs/moq-mux/fuzz/regressions/` and passes.

## Plan

Find how an `Ahead` gets a `scan` cursor behind its `at` (the input offset of
the packet that showed the step) from the input in the
[findings comment](https://github.com/moq-dev/moq/pull/7313#issuecomment-6520535160),
and fix the cause rather than saturating the subtraction. A unit test in
`step.rs` reproduces it with the module's `pes` helper.

Public API: none. Wire: none.
