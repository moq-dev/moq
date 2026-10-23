# [S] Auth and clock names

## Goal

Refusal and reason types have distinct names across moq-auth and moq-net,
and moq-clock and `@moq/clock` mirror each other's names and shapes.

## Plan

Decided 2026-10-22 (API audit):

- "Refusal" names three things (`moq_auth::Refusal`, the policy enum
  `moq_auth::serve::Refusal`, `moq_net::stats::Refusal`) and "Reason" two
  (`refusal::Reason`, `lease::Reason`). Rename the policy enum
  `serve::Denial` and the stats one `RefusalKind`, or similar.
- Rust `moq_clock::anchor() -> (Instant, Wall)` (ns) versus JS
  `origin(): Wall` (ms): name both `origin()` and document the unit split.
  Rust `Manual::install` returns a handle while JS returns a closure: match.
- moq-net exposes both `time::now()` and `Instant::now()`; keep one.
- moq-auth's public times (`Claims`, `Grant::expires`) are `SystemTime`;
  add them to [wall time](/quest/m1/time/wall.md).

Public API: moq-auth, moq-net, moq-clock, `@moq/clock` renames. Wire: none.
