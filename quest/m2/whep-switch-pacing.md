# [S] A WHEP switch waits for a lagging rung

## Goal

A WHEP media line switching to a rendition whose keyframes arrive more than
one frame behind the current one still switches, instead of being starved
by the current rendition running ahead.

## Plan

Found in review of [#5471](https://github.com/moq-dev/moq/pull/5471)
(2026-10-13). While a switch is pending, the current rendition runs one
frame behind, so a new keyframe at the held frame's time can take over.
That helps only if the new rung's keyframe lands within one frame interval;
a rung that lags further every time can still starve the switch.

Decided 2026-10-13 (maintainer): ship the one-frame hold in #5471 and plan
this. Pace the current rendition to the new rung's progress while a switch
is pending, releasing it at the new rung's timestamps. Bound what that
costs: the line takes on the new rung's latency before the switch, and must
not freeze if the new rung stalls (abandon the switch or repick).

Regression with mocked time: a new rung that lags two or more frames still
takes over at its first keyframe.

Public API: none. Wire: none.
