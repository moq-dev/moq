# [M] JS received copies fill what they missed

## Goal

A JS subscriber session asks upstream for the groups its received copy
missed, as Rust's lite subscriber `requested_fill` loop does, so a JS reader
behind a JS or Rust publisher, or a JS relay, gets history its copy never
held. It carries Rust's below-start rule and head fill, and `Fill.time`
resolves a time start that no held group resolves. Verified JS-to-JS and
through a JS relay to Rust.

## Plan

Decided 2026-11-03 (maintainer, follow-up planning after the 2026-11-03 /quest-afk and /quest-complete runs), milestone:

- m0
- ✅ m1, in this epic
- m2

The maintainer chose to plan these separately from
[moq-dev/moq#7696](https://github.com/moq-dev/moq/pull/7696), which lands
JS's on-demand request surface (`Track.Requests`, `Track.Fill`) and deletes
[JS ranges](/quest/m1/subscribe-ranges/js.md). There, received copies never
fill: no handle can reach one.

- The session acts as its received copy's `Requests` handler and asks
  upstream with lite FETCH, or a ranged lite-07 SUBSCRIBE.
- Below-start rule from [moq-dev/moq#7168](https://github.com/moq-dev/moq/pull/7168)
  (decided 2026-10-29, maintainer): give up a run below a start already
  resolved under a budget at least as loose as the reader's, and on a pre-06
  wire fill below the start only when the reader names a floor.
- Head fill from [moq-dev/moq#7625](https://github.com/moq-dev/moq/pull/7625):
  hand out a cached group missing its first frames only once a fill answers
  from frame 0, and give up one whose fill is refused.
- `Fill.time` for a time start no held group resolves, matching Rust's
  `Fill::time`.
- Tests: JS publisher to JS subscriber, and a JS relay between a Rust
  publisher and a Rust subscriber, each missing groups below and inside the
  copy.

Public API: none expected beyond #7696's surface. Wire: none.

## Required

- [JS ranges](/quest/m1/subscribe-ranges/js.md) - the request surface this answers through
