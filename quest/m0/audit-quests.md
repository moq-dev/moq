# [M] Quest tree audit

## Goal

The quest tree agrees with itself and with `main`: done, stale,
conflicting, and misranked quests are fixed.

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

Run `/quest-audit`. After a release cut shifts the milestones, also check
that the move left no broken links or misplaced quests. Until now, audits ran
ad hoc (2026-09-30, 10-05, 10-08, 10-10).
