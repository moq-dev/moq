# [M] Test coverage audit

## Goal

Behavior that matters but no test exercises is covered or planned, and every
test runs in CI.

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

Starting points (2026-10-19): nightly measures only moq-quic, through
`just rs coverage <crate>` (`cargo llvm-cov`). Extend that recipe to the
remaining crates, pick a tool for `js/`, and record per-crate baselines
here. Look for `#[ignore]` tests and JS tests
that no CI job runs (AGENTS.md wires every test into CI, at least nightly),
fixes merged since the previous run without a regression test, and gaps in the
`just test interop --all` matrix.
