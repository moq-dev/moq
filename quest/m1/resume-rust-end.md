# [S] Rust resume holds a declared end across copies

## Goal

A resumed Rust track (`resume::Reader` in `rs/moq-net/src/model/resume.rs`)
keeps a declared end once any copy reported it, as JS does with
[moq-dev/moq#6136](https://github.com/moq-dev/moq/pull/6136). A route that
flaps holds no more replaced copies than the distinct routes it needs, and a
replaced copy is kept only for groups inside the reader's window.

## Plan

Decided 2026-10-21 with the maintainer: one quest for three Rust resume gaps.

- **Declared end.** `poll_finished` reads only the serving copy, so an end
  declared on a replaced copy is lost after a swap. Answer from the first
  declared end, and refuse a copy declaring a different one, matching JS.
- **Flapping routes.** Swapping between the same two routes adds one replaced
  copy per swap, redundant as backups
  ([moq-dev/moq#6141](https://github.com/moq-dev/moq/pull/6141)). Dedupe them
  per swap.
- **Window.** The "may still land" check ignores the reader's `Window`, so a
  sparse-range reader keeps a replaced copy up to its budget for a gap outside
  its ranges (#6141). Judge it inside the window.
- **Route failures.** Rust `route_failed` is narrower than JS
  `ROUTE_FAILURES` (`js/net/src/resume.ts`), and adding the JS guard in Rust
  hangs `partial_group`
  ([moq-dev/moq#6143](https://github.com/moq-dev/moq/pull/6143)). This belongs
  to the recovery-verdicts quest planned in
  [moq-dev/moq#6102](https://github.com/moq-dev/moq/pull/6102); fold it there
  if that lands first, otherwise align the two here.

Regression test per gap, on a mocked clock, mirrored in JS where JS lacks one.

Public API: behavior only. Wire: none.

## Related

- [Resumed close](/quest/m1/resume-closed.md) - the JS half of a declared end across copies
- [Resumed datagrams](/quest/m1/resume-datagrams.md) - the other resume reader fix in the same file
