# Compressed tracks

## Goal

Opaque tracks, compressed per group or not, are published and consumed the
same way from every language, not only Rust and JS, and the bytes on the wire
are identical across all of them.

## Plan

`moq-flate` and `@moq/flate` absorb `moq-binary`'s snapshot and stream modes
in [moq-binary folds into moq-flate](/quest/m1/flate-binary.md), so the crate
already owns the per-group window a caller could otherwise desynchronize. The
track wrapper this line once planned was dropped for that reason. What
remains is reaching those tracks from the hand-written binding wrappers.

No wire, catalog, or relay impact. Compression stays invisible to `moq-net`;
a compressed track is announced, routed, and cached like any other.

## Required

- [Bindings](/quest/m2/flate/bindings.md) - the hand-written wrappers expose flate tracks
