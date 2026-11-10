# [S] .github/ffi.md holds rules, not evidence

## Goal

`.github/ffi.md` states the FFI release layout and its rules in about a
sixth of its 264 lines. The one-off evidence moves out: SHA comparisons,
runner image versions, OpenH264 assembly differences, and runner-minute
tables belong to the PRs that measured them.

## Plan

Planned 2026-11-09 (maintainer, /quest-plan from the deletion audit).
Decided: ✅ trim (rejected: leave as is).

Link the PR for any evidence a rule still rests on. Drop the temporary rule
that accepts legacy per-language workflow names for a tag cut from a
`release` older than the coordinator once `release` carries the
coordinator; if it does not yet, keep that rule and say when it goes.

Public API: none. Wire: none.
