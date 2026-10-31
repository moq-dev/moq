# [M] Declare each check module once in the dispatcher

## Goal

`sh/dispatch.sh` declares each module in one `module` registration call
(scope pattern, tools, whether it compiles Rust, lane, runs alone) that fills
the tables it reads today, instead of listing the module separately in
`scope`, `tools`, `compiles`, `lane`, `alone`, and the lane order. Selection
and scheduling are unchanged. No public API or wire impact.

## Plan

Decided 2026-10-27 (maintainer interview, from the 2026-10-27 refactor audit):
stay in bash with a registration function; porting the dispatcher to Python
was declined.

- Keep each table's comment with the module it explains.
- Preserve the Bun lane (concurrent `bun install` breaks), the shared C++
  build lane, and Dart running alone.
- Prove equivalence with the existing `sh/dispatch-query.test.py` and
  `sh/changed.test.py`, plus a before/after diff of `just ci-select` output for
  a few representative diffs.
