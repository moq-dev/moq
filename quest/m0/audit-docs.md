# [M] Documentation audit

## Goal

Docs match the code: stale pages, broken examples, broken links, and
undocumented exported symbols are fixed or planned.

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

Follow `doc/AGENTS.md` for what belongs in `doc/`. Starting points
(2026-10-19): check every CLI example against `--help`, and walk the
Cross-Package Sync table in AGENTS.md for changes merged since the previous
run that skipped a target. `#![warn(missing_docs)]` is set in 12 of 42
crates; find the published ones without it. VitePress
checks internal links only; nothing checks external ones (try `lychee`).
