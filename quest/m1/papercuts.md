# [S] Papercuts from the 2026-09-30 spawn

## Goal

Two small fixes found while landing the m0 quests: `@moq/net` refuses to
serve a broadcast it did not produce, and `remote_wake_unparks` passes under
load.

## Plan

- JS republish: `@moq/net` publishes only what it produces (decided with
  moq-dev/moq#4599), naming its own origin. Requests already skip received
  routes (`#demand` resolves through `bestEntry(path, received)` in
  `js/net/src/origin.ts`), so the gap is an app handing a consumed
  `broadcast.Consumer` (one a session delivered) back to an origin for
  serving, e.g. `Request.accept(consumer)`, which labels upstream content
  with the local origin's hop. Refuse that loudly at the point it enters the
  origin, not in the lite publisher's `local(..) ?? demand(..)` resolve.

- `rs/moq-uring/src/worker.rs` `remote_wake_unparks`: observe that the worker
  parked instead of sleeping 50 ms and asserting elapsed wall time, per the
  rule that unit tests mock time.
- Dropped in the 2026-10-05 audit: the `.scratch/` formatter excludes. #4598
  retired the agents' scratch-clone convention, and the only writer left is
  the X11 capture benchmark's log, which no formatter touches.

Public API: none. Wire: none.
