# [S] No large future in the FFI layers can overflow a runtime or foreign thread's stack

## Goal

Every future moq-c and moq-ffi spawn or block on is below a size bound
that a lint enforces, so unoptimized builds stay inside the runtime thread
stack (2 MiB) and the 512 KiB foreign threads Apple callers use, with
margin. A future over the bound is boxed or split, not allowed by default.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31 /quest-complete run), from [moq-dev/moq#7090](https://github.com/moq-dev/moq/pull/7090).
Merging it pushed moq-c's `a_failing_dial_reports_its_failure` past the
2 MiB runtime stack in a debug build: unoptimized code copied the session
task's `select!` future into each wrapper's frame (`Background::watch`
twice, `isolate`, `run`, the task), about 2 MB in all. Boxing it, as
moq-tokio's dial futures already are, cut the need under 768 KiB. Other
futures in the FFI layers may sit just under the edge the same way.

- Turn on clippy's `large_futures` for moq-c and moq-ffi (in their
  `[lints]`, or crate attributes, since both inherit the workspace lints)
  with a `future-size-threshold` chosen from what the audit finds, and box
  or split what it flags at the spawn or `block_on` boundary rather than
  deep inside moq-net. The lint measures futures where they are awaited, so
  it can miss one handed straight to a spawn or `block_on`; check those
  calls by hand (or with a `size_of_val` assertion in a test) and record the
  sizes.
- Measure the worst remaining case: run each crate's tests in a debug build
  on a smaller stack (the existing
  `accept_on_a_foreign_thread` in `rs/moq-ffi/src/server.rs` uses
  512 KiB) and record the margin.
- If the lint proves useful, consider it workspace-wide in
  [audit lints](/quest/m2/audit-lints.md) instead.

Public API: none. Wire: none.
