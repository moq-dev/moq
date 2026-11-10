# [M] The recurring audits are fewer and carry plans, not results

## Goal

m3's 13 recurring `audit-*.md` quests become about five, each a Goal plus a
short "check first" list. Results and baselines live in the PR that measured
them, not in the plan.

## Plan

Planned 2026-11-09 (maintainer, /quest-plan from the deletion audit).
Decided: ✅ merge the audits (rejected: keep 13 separate recurring audits).

The 13 files total about 23.6k words, mostly result tables from past runs
(`audit-ci.md` alone is about 7,400 words describing one Nightly).
Several overlap: dead code against simplify, CI against test cost, and deps,
size, and compile time all trace back to dependencies. The same ~150-word
recurrence paragraph is copied into each.

Recommended grouping, the implementer's call:

- Cost: CI, test cost, compile time, artifact size, dependencies.
- Code: dead code, simplification, lints.
- Tests: coverage.
- Docs.
- Interop.
- Performance.

Delete [quest tree audit](/quest/m3/audit-quests.md); `/quest-audit` does
that job. Keep each merged audit's "decided" carve-outs and the still-useful
"check first" items. Drop result tables, measured baselines, and history,
linking the PR that recorded them instead. Each merged audit keeps the
recurrence rule (successor added by the PR that finishes a run, behind the
[release cut](/quest/m3/release/cut.md)), stated once per file in a line or
two. Update m3's README links and anything else that links a merged file.

[CI lane trims](/quest/m0/ci/trim.md) edits `audit-ci.md` rows; whichever
lands second adapts.

Public API: none. Wire: none.
