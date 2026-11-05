# [S] A CBR import's muxRate can't stick low

## Goal

`moq import ts` never keeps publishing a CBR `muxRate` below what the feed
carries because one measurement window lost a few packets: a stable window
that beats the published rate by more than about one packet raises it. When
`moq export ts` misses a decode deadline at its padded rate, its error names
the rate the content needed.

## Plan

Reported in [moq-dev/moq#7958](https://github.com/moq-dev/moq/issues/7958)
and verified against `main`: `Meter::judge`
(`rs/moq-mux/src/container/ts/mux_rate.rs`) publishes the first stable 2 s
window and replaces it only past `DRIFT` (1%). A window about 63 packets short
(an SRT reconnect under 1 s, so no reset) is still stable, so the import
published 24,952,666 b/s for a 25 Mb/s feed and kept it for days. A
supervised restart reuses that value (`Meter::measured`). An export padding to
it with no source stuffing to give back missed a deadline every 10 to 20
minutes, once its `--delay` was used up.

Decided 2026-11-05 (maintainer, /quest-import):

- ✅ Asymmetric hysteresis on the import. Lost or late packets only ever lower
  a window, so a stable window above the published rate by more than about
  one packet per window replaces it; a lower one replaces it only past
  `DRIFT`, which keeps DRIFT's anti-churn intent. It converges in one good
  window and republishes about once, and restart reuse carries the corrected
  value. Not chosen: the reporter's cumulative average over every stable
  window since the last discontinuity (approaches from below, republishes
  several times early), or both combined (more state).
- ✅ The export keeps failing on a missed deadline, and its `missed()` error
  (`schedule.rs`) adds the rate the content needed, from the padded rate plus
  the backlog over the elapsed time. Not chosen: a backlog-slope detector that
  fails before the delay runs out (a window and threshold to tune for a case
  the import fix removes), or the export raising its padded rate itself
  (breaks the CBR contract that two exporters lay a slot the same way).

Tests: a meter fed one short window, then exact ones, publishes the exact rate
after one window and holds it; a window below by under 1% still doesn't
republish. The export's deadline-miss error carries the needed rate.

Public API: none. Wire: none; the catalog's `mpegts.muxRate` gets more
accurate.

## Closes

- [#7958](https://github.com/moq-dev/moq/issues/7958) - `import ts` keeps a CBR feed's `muxRate` from one 2 s window
