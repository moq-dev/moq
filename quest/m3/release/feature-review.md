# [L] Feature review before the cut

## Goal

Every major feature or work line that landed since the last release, with or
without a new API, has been judged end to end: whether it earns its
complexity, holds its stated contract, is tested and documented, and should
ship as is. Each line that needs work has a follow-up quest that lands before
the cut, or a recorded decision to ship it as is.

## Plan

Recurs once per API or wire cut (decided 2026-10-19; per breaking cut
2026-11-09). The [release cut](/quest/m3/release/cut.md) re-creates this quest
when the maintainer names the next API or wire cut, so the PR finishing this
run adds no successor.
Record what this run learned (inventory method, findings, what to check first)
in the cut's PR notes or a follow-up quest, so the next run starts from it.
Only `/quest-delete` ends the recurrence.

Decided 2026-11-12 (maintainer, /quest-audit before the cut): ✅ this run
starts now, on what has landed, with a delta pass after m2 (rejected: one pass
after m2 lands, which puts every finding behind the routes chain instead of
beside it). The first pass's PR rewrites this quest to the delta pass: what
m2 landed since, with [m2](/quest/m2/README.md) under `Required`. The cut
waits for the delta pass. Inventory the work lines from the
epics and quests deleted since the last release and from `git log` by area.
Judge each one adversarially (AGENTS.md) and challenge its premise, not only
its execution: is the feature worth supporting at all, and would a simpler
mechanism the stack already has deliver most of it? Then look for simpler
designs, code that can be deleted, tests that cost more than they catch, and
claims without evidence.
File follow-ups through `/quest-plan` into m1 or m2, or ask the maintainer to
ship as is.

The first run must cover the MPEG-TS work (decided 2026-10-19 instead of a
separate TS audit): everything since the 2026-10-11 TS audit, judged against
the contracts in the [MPEG-TS guide](/doc/bin/ts.md).
Question each TS capability's premise the way the move to distinct importer
epochs dropped active-active publishers.
