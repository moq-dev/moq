# [M] Relay demand gaps left by the widened-age re-offer

## Goal

Two gaps found by [moq-dev/moq#6717](https://github.com/moq-dev/moq/pull/6717)
are fixed or ruled out, each with a test:

- On lite before 07, a reader with a narrow max age receives the whole backlog.
- A relay newcomer may pull one group upstream twice: once from its range fill
  and once from the publisher's re-offer.

## Plan

Reproduced through a relay on 2026-10-28 (10 groups one second apart, a 2.5s and
a 6.5s reader, publisher → relay → viewers):

- **Backlog.** On lite-05 and lite-06 a 2.5s reader gets all 10 groups, direct
  or through a relay; lite-07 gives it 6..9. Those copies are untimed (their
  timestamps may be send times) and their FETCH carries no max age, so the
  backfill below the copy's start is never judged against the reader's budget.
  Lite-06 resolves the start from the budget (SUBSCRIBE_START says 6), so the
  live part is right and the FETCH backfill of 5..0 is the excess. Lite-05
  starts at the live edge, so nothing on its wire bounds the backlog.
  The same excess shows on moq-transport, whose copy is timed: a filled group is
  judged by its live successor only (`TrackState::successor` skips backfill),
  so every backfilled group looks fresh.
- **Double pull.** A warm lite-07 relay pulls 9 upstream groups for 8 distinct,
  with or without 50ms of link latency: the newcomer's fill FETCHes one
  below-start group while the widening's re-offer sends it too. It happens once
  per widening of the relay's aggregate max age, not per newcomer
  (`widened_max_age::wider_newcomers_after_the_first_pull_nothing_upstream`).
  On lite-06 a widening re-offers 4 groups the relay already backfilled, a
  consequence of the backlog gap.

Open: whether lite-06 bounds a copy's backfill by the publisher's resolved
start, whether lite-05 is ruled out, and whether the one-group race is worth a
fix. See [moq-dev/moq#7052](https://github.com/moq-dev/moq/pull/7052).

Public API: none expected. Wire: none expected.
