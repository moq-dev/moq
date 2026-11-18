# [S] Rust resume copies match @moq/net's relative starts and capped ends

## Goal

A Rust reader over a lite-07 received copy resolves a `Relative` range the
way `@moq/net` does: counted back from the start the publisher reported in
SUBSCRIBE_START, not from the groups the copy happens to hold. A sparse
request mixing a time range and a relative one reads every group it names
instead of stalling after the first. A route capped while replaced and then
returned to reads on to the reader's own bounded end.

## Plan

Decided 2026-11-17 (maintainer, on
[moq-dev/moq#9027](https://github.com/moq-dev/moq/pull/9027)): ✅ keep the
JS fix and port it here, so the mirror diverges only briefly (rejected:
fixing Rust in that PR; reverting JS to Rust's behavior).

- Reproduce first: a reader asking `[{time:1..2}, {relative:2..time:8}]`
  over a lite-07 copy holding only part of the track. In JS it read group 0
  and stalled, because the copy counted back over only the groups it held.
- `Holdings::back` for a received copy (`rs/moq-net/src/model/track.rs`)
  counts the copy's own cache; resolve a relative start from the reported
  start instead, as `@moq/net`'s `relativeStarts` hook
  (`js/net/src/track.ts`, `js/net/src/internal.ts`) does.
- Scope each range's report to the latest it was counted from, recorded when
  its start was answered (SUBSCRIBE_START's Largest, or the copy's latest at
  SUBSCRIBE_STARTED), as JS does: once a newer group arrives, a late live
  join counts from the copy's own latest instead of replaying from the old
  report, and an update that changes no range never re-stamps it.
- `Copy::wire` (`rs/moq-net/src/model/resume.rs`) takes
  `sub.resolved_ranges()` for unchanged ranges, which is the resolution of
  the copy's clamped wire: a cap while replaced likely becomes the range's
  end for good. JS keeps each range's first resolution instead
  (`js/net/src/resume.ts` `#wire`, test "a returning route keeps the
  reader's bounded end across its cap"); reproduce and port.
- If Rust turns out to handle a case elsewhere, delete this quest with a
  test that pins it.
