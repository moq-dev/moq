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
the JS ranges quest. There, received copies never
fill: no handle can reach one.

- The session acts as its received copy's `Requests` handler and asks
  upstream with lite FETCH, or a ranged lite-07 SUBSCRIBE.
- Below-start rule (maintainer, 2026-11-04, replacing the 2026-11-03
  instruction to port [moq-dev/moq#7168](https://github.com/moq-dev/moq/pull/7168)'s
  budget inference): below a resolved start, judge history against the
  reader's own budget and fill what it reaches; lite-06 relies on re-offer,
  and pre-06 fills below the start require a named floor. This mirrors
  current Rust and the
  [2026-10-31 decision in moq-dev/moq#7235](https://github.com/moq-dev/moq/pull/7235#issuecomment-6540213825):
  a SUBSCRIBE_UPDATE can cross SUBSCRIBE_START, so the copy cannot know which
  budget selected the start.
  - ✅ Mirror current Rust and the #7235 decision.
  - Keep #7168's inference, accepting the crossed-update problem.
- Head fill from [moq-dev/moq#7625](https://github.com/moq-dev/moq/pull/7625):
  hand out a cached group missing its first frames only once a fill answers
  from frame 0, and give up one whose fill is refused.
- `Fill.time` for a time start no held group resolves, matching Rust's
  `Fill::time`.
- Tests: JS publisher to JS subscriber, and a JS relay between a Rust
  publisher and a Rust subscriber, each missing groups below and inside the
  copy.

Public API: none expected beyond #7696's surface. Wire: none.
