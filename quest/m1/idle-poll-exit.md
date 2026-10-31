# [M] An idle track poll exits cheaply

## Goal

A reader's poll on a track with nothing new costs a small constant number of
instructions, not about 3,800, so catalog and media fanout scale with the
readers that have work rather than every pending reader.

## Plan

Found 2026-10-28 by [moq-dev/moq#7140](https://github.com/moq-dev/moq/pull/7140),
which moved the arrival-grace and clock checks behind the cheap "nothing
changed" checks in `Misses::judge`. Even at the old baseline, a pending catalog
poll costs about 3,800 instructions per reader, spread across the cursor's
poll path (`Cursor::probe` in `rs/moq-net/src/model/track.rs` and what it
calls) on a track with nothing new.

Decided 2026-10-28 (maintainer): m1, for relay fanout scale.

- Profile one idle poll with instruction counts (callgrind or
  `perf stat --no-scale`) and find where the cost goes.
- Add one early exit the whole path shares, keyed on something every change
  already bumps (for example a track revision the cursor last saw), rather
  than a short-cut per function.
- Measure the moq-mux catalog fanout benches and the moq-net session benches
  over tracks and readers, before and after.

Public API: none. Wire: none.

## Related

- [Mesh session cost](/quest/m2/perf/mesh-session-cost.md) - per-session costs in the same model
