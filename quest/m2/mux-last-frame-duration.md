# [XS] The last frame of a group keeps its duration on native-scale imports

## Goal

A group's final frame carries a duration whatever the source timescale.
`container::Producer::flush` subtracts the boundary from the frame's
timestamp with `Timestamp::checked_sub`, which refuses mismatched scales;
the TS importer feeds 90 kHz and the MKV importer nanoseconds, every
`cut()` passes `None`, so the boundary is micro-scale and the final frame
silently keeps `duration: None`. fMP4 export then infers it.

## Plan

Compute in micros and convert back to the frame's scale, or reuse
`close_duration` in `container/mod.rs`, which already does this safely.
Regression: 90 kHz frames, `cut(None)`, assert the last frame's duration is
`Some`. Public API: none. Wire: none.

## Required

- [Merge dev](/quest/m1/merge-dev.md) - the dev-only producer this fixes
