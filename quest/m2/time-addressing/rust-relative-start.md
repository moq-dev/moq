# [S] Rust lite-07 copies count a relative start from the publisher's

## Goal

A Rust reader over a lite-07 received copy resolves a `Relative` range the
way `@moq/net` does: counted back from the start the publisher reported in
SUBSCRIBE_START, not from the groups the copy happens to hold. A sparse
request mixing a time range and a relative one reads every group it names
instead of stalling after the first.

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
- If Rust turns out to handle the case elsewhere, delete this quest with a
  test that pins it.
