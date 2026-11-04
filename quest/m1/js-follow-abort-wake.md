# [S] A routed JS read wakes when its track aborts

## Goal

In `@moq/net`, a pending `group.readFrame()` on a group of a broadcast found
through `Origin.Consumer.request()` or `follow()` rejects as soon as the
publisher calls `track.abort(err)`, with the track's error, as it already
does through `broadcast.consume()`, instead of staying parked until cleanup.
A regression test covers both paths, and Rust is checked for the same gap.

## Plan

Decided 2026-11-03 (maintainer, follow-up planning after the 2026-11-03 /quest-afk and /quest-complete runs), milestone:

- m0
- ✅ m1
- m2

Found by [moq-dev/moq#7693](https://github.com/moq-dev/moq/pull/7693) (merged): the
pending read stays parked until the next read throws the error, while a
pending `nextGroup()` wakes on both paths.

- Likely in `js/net/src/resume.ts`: a group handed out from a route's copy
  isn't aborted when the source copy fails. Abort every handed-out group with
  the copy's error.
- Check Rust's resume reader for the same case (a frame read pending on a
  handed-out group when the serving track aborts) and fix it there too if it
  hangs.
- A repro: moq-boy's status loop reading through the origin, where the
  abort leaves its read parked until cleanup (the maintainer approved this
  quest again from #7693's iterate, 2026-11-03).
- The test fails without the fix: publish, route a read through
  `request()`/`follow()`, park a `readFrame()`, abort the track, and expect
  the rejection without another read.

Public API: none. Wire: none.

## Related

- [Resume redesign](/quest/m2/resume-redesign.md) - would replace this code later
