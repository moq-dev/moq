# [XS] A burst of damaged TS units logs a summary, not a flood

## Goal

When MPEG-TS ingest refuses damaged units, it warns on the first damaged unit
of a burst per PID, then logs one summary with the count per interval, instead
of one warning per unit for the whole session. Lands on `main` first, then is
cherry-picked to `release`.

## Plan

`Import::damage` (`rs/moq-mux/src/container/ts/import.rs`) already counts per
PID in `damaged`, but warns on every unit. Since #4733 (and #5124 on
`release`) a sender streaming garbage no longer ends the session, so the
warning repeats for its lifetime. Decided 2026-10-10: first warning plus a
periodic summary per PID, no new dependency; test it with mocked time.

Public API: none. Wire: none.
