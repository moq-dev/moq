# [S] js/net: declare a track end ahead of the live edge and observe it

## Goal

A browser publisher can end a track at a declared boundary ahead of the live
edge, and a browser consumer can await that boundary, matching Rust's
`finish_at` and `finished()`.

## Plan

Additive on `@moq/net`, so it belongs on main; main gains `final()` with the
dev merge, so start after that.

- Add `Track.Producer.finishAt(final)`, mirroring `finish_at`
  (`rs/moq-net/src/model/track.rs:1322`): the boundary must exceed the highest
  produced sequence, groups below it are still accepted, groups at or above it
  are refused.
- The lite subscriber drains SUBSCRIBE_END and drops the sequence
  (`js/net/src/lite/subscriber.ts:744-757`). Feed it into the consumer's
  existing `final()` (`js/net/src/track.ts:983-991`) so a remote clean end is
  observable before the live edge reaches it.
- Add an awaitable `finished()` twin of `final()`, mirroring Rust
  (`track.rs:3540`): it resolves with the boundary once known and rejects on
  abort.

The rest of #2318 landed: SETUP role (`js/net/src/lite/setup.ts:19-93`), typed
`SessionError` and `StreamError` with code registries
(`js/net/src/error.ts:15-236`), `startAt` and `endAt` (`track.ts:1034`,
`:1043`), `latest()` (`:979`), `payload` on every frame type, and the dead
`SubscribeOptions` export is gone. The producer-side prefix announce
(`createBroadcast`, `announce(route)`, `dynamic(pattern, route)`) landed.

## Required

- [Merge dev](/quest/m1/merge-dev.md) - builds on dev-only code that reaches `main` with the merge

## Closes

- [#2318](https://github.com/moq-dev/moq/issues/2318) - close this issue when the quest finishes
