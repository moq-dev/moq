# [M] Closing a binding broadcast ends its media

## Goal

No moq-ffi or moq-c media track ends without a cause. Closing a broadcast
ends every media handle still open with `Unroutable`, and later calls on
those handles return `Closed`. A handle freed or dropped without `finish()`
aborts with `Unroutable` too. `finish()` stays the only clean end. Readers,
including the TS export's follower, see the publisher leave instead of a
cause-less drop.

## Plan

Decided with the maintainer on 2026-10-24, breaking the API: structured
ownership, no new `abort()` on media handles.

- Found while fixing [moq-dev/moq#6656](https://github.com/moq-dev/moq/pull/6656):
  `MoqBroadcastProducer::close()` and `moq_publish_close()` leave media
  tracks open, and the handles have no abort. An app that drops them while
  its session is up leaves readers seeing the tracks dropped unfinished, which
  `left()` does not count as the publisher leaving.
- Media handles are children of their broadcast. Close ends the children
  first, then the broadcast. A drop without `finish()` is an abort, never a
  cause-less drop, so a garbage-collected binding cannot leak one.
- Breaking: a call on a media handle after its broadcast closes returns
  `Closed`, and a dropped handle now aborts its track. Migrate each binding
  wrapper (py, go, swift, kt, dart, cpp), the moq-ffi and moq-c doc
  comments that say subscribed tracks carry on, and `doc/lib/*`; the PR's
  Impact says how.
- Test in moq-ffi and moq-c with a paused clock: close with open handles,
  drop without finish, and finish then close, each read by a subscriber that
  checks the cause.

## Required

- [Stateless broadcasts](/quest/m1/broadcast-lifecycle/stateless.md) - lands the catalog-only finish this builds on
