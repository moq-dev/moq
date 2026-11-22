# [S] A moq-transport reader from the oldest group gets the backlog

## Goal

A subscription whose ranges start at `track::Start::Oldest` asks a
moq-transport publisher for its held groups, not a live join, so it is
delivered the backlog the publisher holds (bounded by what the publisher
retains). A test pins the wire request.

Non-goals: moq-lite, which asks for Oldest natively.

## Plan

Decided 2026-11-22 (maintainer, /quest-plan): ✅ plan it (rejected: accept
live-only for Oldest readers over moq-transport).

Found on [moq-dev/moq#9265](https://github.com/moq-dev/moq/pull/9265), where
the reader was one that left Min Age unset. It still holds on #9316's head
(4b19a51169): `wire_bounds` in `rs/moq-net/src/ietf/subscriber.rs` asks from
group 0 only when some range has a time (`first.map(|_| 0)`), and
`Subscription::start()` is `None` for `Oldest` (`model/subscription.rs`,
`range.time()?`). So an Oldest-only subscription yields `(None, None)`, and
`subscribe_join` turns that into `NextObject` with a relative fill of the
current group (a joining FETCH on draft 18). No test covers it; the nearest
covers `Live` and `Time` ranges.

Recommended: any non-empty `Oldest` range makes the wire start `Some(0)`,
which `subscribe_join` already sends unfiltered with the copy's fills fetching
the groups below the live group. That holds when it is mixed with a `Live`
range too, since the union must reach back. Add a `wire_bounds` test for
Oldest-only and Oldest mixed with Live, and an end-to-end test that an Oldest
reader over moq-transport receives groups published before it subscribed.

[Max Age is a Live range's staleness budget](/quest/m1/live-max-age.md) may
redefine a None start; keep `Oldest` meaning "the oldest group the publisher
holds" either way.

## Waiting

- moq-dev/moq: [#9316](https://github.com/moq-dev/moq/pull/9316) merges; it
  replaces the Min Age shape this fix targets with `Start::Oldest`. Check with
  `gh pr view 9316 --json state`.

## Related

- [Max Age is a Live range's staleness budget](/quest/m1/live-max-age.md) - decides what a None start means on lite-07
