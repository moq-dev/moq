# [S] Front copy-walk benchmark

## Goal

A benchmark sweeps tracks per front against copies per track for the walks a
front makes over its copies when its route changes, so a cost that grows with
the front shows up as a slope.

## Plan

Decided in the 2026-10-05 audit: re-scoped from the wildcard line's admission
walk, which no longer exists (`Provenance::admit`, `front.admit()`, and
`io.copies()` are on neither `main` nor the line branch), to the copy walk
#4741 put on `main`. The Wildcard blocker is dropped, since this code is on
`main`.

- Since #4741 a front resumes route changes by reading the routes' copies.
  `attach` and `source_closed` call `drain_copies`
  (`rs/moq-net/src/model/front.rs`), which walks every track, and the driver
  keeps the serving source's copies until End. Nothing benchmarks either.
- Extend `rs/moq-net/benches/origin.rs` (or `session.rs` if a session is
  needed to drive it) with a route swap swept over tracks per front and copies
  per track.
- If the slope matters, say so rather than optimizing here;
  [Front deadlines](/quest/m1/front-deadline-index.md) owns per-track wakes.

Public API: none. Wire: none.

## Related

- [Front deadlines](/quest/m1/front-deadline-index.md) - per-track wakes on the same front
