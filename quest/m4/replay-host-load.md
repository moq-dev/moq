# [S] The mesh replay reports the host's CPU pressure per event

## Goal

The mesh replay's report shows, for each event, how much the host was starved for
CPU while it ran, read from Linux PSI (`/proc/pressure/cpu`), so a violation from a
CPU-starved relay doesn't read as a defect in the build under test. Today
`test/mesh-replay/replay.py` records no host load, and earlier violations from
CPU-starved runs were misread (see the 2026-11-19 and 11-20 notes in
[Nightly](/quest/m0/nightly/README.md)).

## Plan

Scope: one per-event column in `report.md` and the same value in `report.json`
(`COLUMNS` and `NOTE` in `replay.py`), plus the report section of
`test/mesh-replay/README.md`.

Recommended: read the `some` line's `total` counter (microseconds some task
waited for CPU) when the event starts and when it settles, and report the
difference as a percentage of the elapsed time. That covers exactly the event's
window, where `avg10` would blur across events. Decide at setup whether
`/proc/pressure/cpu` is readable; where it isn't (macOS, or a kernel without
PSI), the column reads n/a for the whole run.

Verify with a case in `test/mesh-replay/replay.test.py` that feeds two
`/proc/pressure/cpu` readings and checks the reported percentage, and a short
local replay whose report shows the column.

Decided 2026-11-25 (maintainer, `/quest-plan`): ✅ keep (rejected: drop).
