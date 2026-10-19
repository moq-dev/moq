# [S] Artifact size audit

## Goal

Growth in release binaries, the FFI library, WASM, and JS bundles since the
previous run is explained, fixed, or planned.

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

Starting points (2026-10-19): the nightly `sizes` job (`sh/size/`) fails
on 5% growth against the previous report or 10% against one 30 days old. It
catches step changes, not creep across a release, so compare against the
baselines recorded here. It already attributes FFI growth with
`cargo bloat`. Related: [FFI size profile](/quest/m2/ffi-size-profile.md).
