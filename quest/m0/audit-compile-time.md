# [M] Compile time audit

## Goal

The crates, dependencies, and code that dominate build time are known, with
recorded baselines, and the worst are fixed or planned.

## Plan

Recurs once per major release (decided 2026-10-19). The PR that finishes
this run also adds the next one with the same file name at the end of the next
milestone's `Required` (m0 to m1, m1 to m2). The successor keeps the Goal
and this paragraph; the rest of its Plan is rewritten with what this run
learned: baselines, results, and what to check first. A successor placed in m2
also lists the [release cut](/quest/m2/release/cut.md) under `Required`, so it
waits for the release in flight to ship. Only `/quest-delete` ends the
recurrence. Fixes land in the run's PR, one commit each, rather than as follow-ups.
Only a change that needs a maintainer decision (public API, scope) or is
independently landable and too large to review alongside the rest becomes a
follow-up through `/quest-plan`.

Starting points (2026-10-19): nothing measures compile time. Record clean
`cargo build --timings` for dev and release, an incremental rebuild of
moq-net, and `just check` wall time. The nightly size job logs release
`Build` step durations without comparing them (`sh/size/README.md`). Look
for heavy proc macros, duplicate dependency versions (`deny.toml` only warns),
monomorphization bloat (`cargo llvm-lines`), and default features that pull
heavy dependencies.
