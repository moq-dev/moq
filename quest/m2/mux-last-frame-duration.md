# [XS] The last frame of a group keeps its duration on native-scale imports

## Goal

A group's final frame carries a duration whatever the source timescale.
`container::Producer::flush` subtracts the boundary from the frame's
timestamp with `Timestamp::checked_sub`, which refuses mismatched scales;
the TS importer feeds 90 kHz and the MKV importer nanoseconds, every
`cut()` passes `None`, so the boundary is micro-scale and the final frame
silently keeps `duration: None`. fMP4 export then infers it.

## Plan

Convert the boundary into the frame's own scale before subtracting, so a
90 kHz frame gets a 90 kHz duration; going through micros and back rounds
(3003 ticks is not a whole number of micros) and fMP4's `trun_duration`
then refuses the inexact conversion. Regression: 90 kHz frames, `cut(None)`,
assert the last frame's duration is exactly the tick count, and encode the
resulting fMP4. Public API: none. Wire: none.

## Required

- [Merge dev](/quest/m1/merge-dev.md) - the dev-only producer this fixes
