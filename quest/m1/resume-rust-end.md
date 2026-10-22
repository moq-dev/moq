# [S] Rust resume holds a declared end across copies

## Goal

A resumed Rust track (`resume::Reader` in `rs/moq-net/src/model/resume.rs`)
keeps a declared end once any copy reported it, as JS does with
[moq-dev/moq#6136](https://github.com/moq-dev/moq/pull/6136). A route that
flaps holds no more replaced copies than the distinct routes it needs, and a
replaced copy is kept only for groups inside the reader's window.

## Plan

Decided 2026-10-21 with the maintainer: one quest for three Rust resume gaps. A fourth, the
narrower Rust `route_failed`, moved to the recovery verdicts work.

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

Regression test per gap, on a mocked clock, mirrored in JS where JS lacks one.

Decided 2026-10-22 (maintainer), settled in
[#6224](https://github.com/moq-dev/moq/pull/6224)'s review: Rust refuses a copy
declaring a different end with the same non-route-failure classification as
JS's `Incompatible` (`js/net/src/resume.ts`). Merge order: #6136, then #6224
(#6209 and #6251 have landed).

Public API: behavior only. Wire: none.

## Related

- [Resumed close](/quest/m1/resume-closed.md) - the JS half of a declared end across copies
