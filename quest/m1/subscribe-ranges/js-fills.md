# [M] JS head fills and time lookups

## Goal

A JS reader hands out a cached group missing its first frames only once a
fill answers it from frame 0, and gives the group up when that fill is
refused, as Rust's head fill does. A time start that no held group resolves
asks the track's `Requests` handler through `Fill.time`, matching Rust's
`Fill::time`. A JS received copy's session serves head fills upstream, as it
serves its group fills.

## Plan

Decided 2026-11-03 (maintainer, follow-up planning after the 2026-11-03 /quest-afk and /quest-complete runs), milestone:

- m0
- ✅ m1, in this epic
- m2

The maintainer chose to plan these separately from
[moq-dev/moq#7696](https://github.com/moq-dev/moq/pull/7696), which landed
JS's on-demand request surface (`Track.Requests`, `Track.Fill`).
[moq-dev/moq#7845](https://github.com/moq-dev/moq/pull/7845) made the lite
subscriber session its received copy's `Requests` handler, over lite FETCH a
group at a time, with the below-start rule below. It tested JS to JS and
through a JS relay between Rust peers (`just test relay-fill`).

- Below-start rule, landed in #7845 (maintainer, 2026-11-04, replacing the
  2026-11-03 instruction to port
  [moq-dev/moq#7168](https://github.com/moq-dev/moq/pull/7168)'s budget
  inference): below a resolved start, judge history against the reader's own
  budget and fill what it reaches; lite-06 relies on re-offer, and pre-06
  fills below the start require a named floor. This mirrors current Rust and
  the
  [2026-10-31 decision in moq-dev/moq#7235](https://github.com/moq-dev/moq/pull/7235#issuecomment-6540213825):
  a SUBSCRIBE_UPDATE can cross SUBSCRIBE_START, so the copy cannot know which
  budget selected the start.
  - ✅ Mirror current Rust and the #7235 decision.
  - Keep #7168's inference, accepting the crossed-update problem.
- Head fill, from [moq-dev/moq#7625](https://github.com/moq-dev/moq/pull/7625)'s
  `Heads`: a bounded window hands out a group missing its head only once a
  fill answers from frame 0, in the subscription's order. It gives up a
  refused head like an unfillable miss, lets go of one its bounds no longer
  admit, and writes off a stale one by its own budget. JS fills name whole
  groups today, and a fill cannot accept a group the cache still holds
  headless, so the fill needs a frame start and that replacement. The session
  FETCHes the head from frame 0 (lite-06+ frame bounds).
- `Fill.time`: the window looks a time start up through the handler before
  reaching back to the oldest group, reading nothing of that range until the
  first accept, as Rust's `Window::looking` does. A session leaves a lookup
  unanswered, as Rust's does, so a received copy's range reaches back as it
  does now.
- Tests: a local handler answering heads and time lookups, and a headless
  group in a received copy filled over FETCH.

Public API: `Fill` gains `time` and a frame start, matching Rust's
`Fill::time` and `Fill::start`. Wire: none.
