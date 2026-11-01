# [S] A stalled reader within budget gets no Old resets through a relay

## Goal

A test proves that a subscriber which stalls before its first read, behind a
relay over a transport with capped stream credit, receives no `Old` stream
resets for groups still within its max-delay budget, and reads them in order
once it resumes.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7342](https://github.com/moq-dev/moq/pull/7342): m2, as
recommended.

[moq-dev/moq#6990](https://github.com/moq-dev/moq/pull/6990) fixed a
`Start::Latest` join that subscribed with only the warmup as its budget, so
the relay reset queued groups as `Old`. [moq-dev/moq#7342](https://github.com/moq-dev/moq/pull/7342) tested the publisher's skip on
the wire budget in moq-audio, but not the transport-backpressure half: the
relay's own resets while the reader withholds stream credit.

- Put it in `rs/moq-relay/tests/integration/` with a relay, a publisher
  bursting a few groups, and a subscriber with a small `max_streams` (see
  `peer_limits.rs:39`) and a max delay larger than the burst, that stalls
  before reading. Assert every stream it receives completes with no
  `StreamError::Old` and that a group past the budget is reset `Old`.
- Drive the stall with the simulated clock, not wall-clock sleeps.

Public API: none. Wire: none.
