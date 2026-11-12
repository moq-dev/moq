# [S] A JS reader's handout costs the fills it touches, not every fill it joined

## Goal

Handing out a group or passing the cursor (`#handOut`, `#raiseNext`, the stale pass in `js/net/src/track.ts`) costs a JS reader the asked runs and fills that change, not every one it holds.
A reader that still waits on many fills while live groups arrive then costs linear work over the stream, not quadratic.

## Plan

- Found on the fill probe change that indexed `FillState` by what each fill still owes: the probe now costs what changed, but `#fillsMoved()` with no range still runs `#clipAsked`, which rewrites `#asked`, `#refilled`, and `#givenUp` in full, and `FillState.moved(asker)` rejudges every fill the reader joined, on every handout.
- Recommended shape: pass `#fillsMoved` the span that stopped being read (the handed-out sequence, what `#forgetPastBudget` forgot, what the cursor passed) so it trims only that and calls `moved(asker, start, end)`, keeping the full clip for `#bound`, where the window itself moved.
- Sweep a bench over the outstanding fills and the readers, as fan-out work requires: extend `js/net/bench/fill-probe.ts` with live groups handed out between drops.
