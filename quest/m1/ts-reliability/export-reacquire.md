# [S] The TS export re-acquires an unmarked timeline step

## Goal

`moq export ts` keeps running when its source's timestamps step with no
restart or skip marker. It re-acquires the clock as at startup, instead of
exiting with a drift error ("the source's clock runs +172.5 ppm off ours").
This protects subscribers of TS publishers other than `moq import`.

## Plan

Traced 2026-10-28 from [#7102](https://github.com/moq-dev/moq/issues/7102): the
+172.5 ppm is noise, not a measurement of the source.

- `jumps()` (`rs/moq-mux/src/jitter.rs`) opens a generation only on a marker.
  An unmarked step goes to `steer()` on the old clock.
- The 600 s rate window drops every pre-step floor, but `startup` survives,
  since only `acquired()` resets it. So `lost` reads as the whole step.
- The bail then fires on the first two-floor rate over 30 ppm (~345 µs of
  arrival jitter across one 2 s step).

Decided 2026-10-28 (maintainer): treat a step outside any marker as a new
acquisition, as `acquired()` does. Keep [#5564](https://github.com/moq-dev/moq/issues/5564)'s
decisions: no minimum span or sample count, and never widen the 30 ppm or
`lost` thresholds. A step is told apart from drift by size: a phase change no
clock rate within the bound could produce in the elapsed time. Check that the
straddling step's slew bound (`MAX_SLEW · elapsed`) cannot swing the drift
across the bound in one measurement.

Tests: a forward and a backward unmarked step each re-acquire and keep
emitting. The #5564 startup cases still pass unchanged.

Public API: none. Wire: none.

## Related

- [A timebase step restarts the TS import](/quest/m1/ts-reliability/step-restart.md) - the importer-side fix, under which `moq import` never forwards such a step
