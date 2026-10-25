# [S] JS tracks end clean only on a deliberate finish

## Goal

In `@moq/net`, a received track or group ends cleanly only when its
publisher deliberately finished it. A local session close, a reload, a peer
protocol violation, or a request the publisher never answered ends the
affected tracks and open groups with an error, as Rust does, so players and
resumed readers recover instead of treating truncation as a finish.

## Plan

Found by the 2026-10-24 pre-cut audit at 151a529670. All three reproduced.

- **Local close truncates open groups** (inherited behavior, new hook):
  `track.ts:1761` closes every cached group with `abort === undefined` when
  `hooks.releaseTrack` (`track.ts:819`, called from `lite/subscriber.ts:1803`
  and `ietf/subscriber.ts:977`) or the public `Producer.close()` runs. A
  group with frame 1 still in flight reads as complete, and a resumed
  reader's `Recover` never refills it. Rust `track::Producer::close`
  (`rs/moq-net/src/model/track.rs:3046`) aborts open groups with a session
  cancel; the JS hook's doc claims to mirror it. Abort open groups with a
  cancel error and keep finished groups readable.
- **A lite protocol violation ends tracks cleanly** (new on main, from
  [moq-dev/moq#5550](https://github.com/moq-dev/moq/pull/5550) and
  [moq-dev/moq#5279](https://github.com/moq-dev/moq/pull/5279)):
  `lite/connection.ts:365` and `:198` call the public `abort()`, which closes
  the subscriber with no error and empties `#subscribes`, so the later
  `close(fatal ?? closeError)` at `:307` does nothing. Add a private
  `#fail(err)` that closes the subscriber with the error before closing the
  transport; the public `abort()` stays clean.
- **An unanswered request ends clean and empty** (inherited):
  `broadcast.ts:152` calls `request.reject()` with no error, and
  `track.ts:387` turns that into a clean end at `{0,0}`. Reject with a
  Dropped-coded error, matching Rust's `model/broadcast.rs:752`, and make
  `Request.reject(err: Error)` require its argument.

The audit traced a family of earlier fixes to the same root: `close()` with
no argument means clean at every layer, and each cancellation source tears
down separately (b99cad9d5c, c83c141166, 2704e10e20, 7f75925ac5,
097dc1ee62, 333d963760). Prefer one session-level terminal cause that every
teardown path reads over three more point fixes, if it stays small; the
larger JS restructure is not worth it ahead of
[rs2ts](/quest/m2/rs2ts/README.md).

Public API: breaking. `Request.reject` requires an error (callers pass one),
and `Producer.close()` aborts groups still open. Wire: a rejected request's
error code changes.
