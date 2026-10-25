# [XS] A route's terminal state is one phase

## Goal

The resume route's "no front will replace this copy" state is one `Phase`
enum with a single `terminal()` check, so no call site can test the wrong
subset of flags again.

## Plan

Found by the 2026-10-24 pre-cut audit at 151a529670. Source-inferred, new on
main.

`Route.end`, `Route.concluded`, and the kio producer's `is_closed()` all mean
the route is done, and the call sites in `rs/moq-net/src/model/resume.rs`
(about lines 210, 309, 324, 1650, 1704, and `Fetching::poll` via `end()`)
each combine a different subset. 109235732c fixed a stalled read spinning
forever because `Recover::wants` lacked `is_closed()`; `poll_changed`'s
closure still omits it and relies on kio returning Ready once closed.

Replace the three with one phase (for example Serving, Parked, Between,
Concluded, Ended(result)), set in `Drop for Producer` too, and route every
check through `terminal()`. Behavior-preserving; the existing resume tests
are the guard.

Public API: none. Wire: none.
