# [M] Upstream requests linger past their demand

## Goal

An upstream FETCH or SUBSCRIBE outlives the last reader wanting it by a short
linger, so a request issued and abandoned in quick succession (a reader that
re-subscribes, seeks, or blips) does not churn upstream. A reader returning
within the linger rides the request already in flight.

## Plan

Decided in planning (2026-10-03, from [#4741](https://github.com/moq-dev/moq/pull/4741)):

- **Both request types.** The session's upstream SUBSCRIBE, today canceled the
  moment the last subscriber leaves, and FETCH, today cut short (all the way
  upstream) once no fetcher waits and no reader holds its group.
- **Demand decides, per request type, unsplit.** A FETCH is canceled once its
  demand stays unused through the linger. A subscription's groups ignore group
  demand: a front's reader still gives up a stale group a fetcher happens to
  hold, as a relay's subscription would. Group demand is not split into fetch
  and subscription halves.
- **A short fixed window**, around a second: enough to absorb a re-subscribe or
  a seek, far below the front's 30 s cache linger. A constant, measured during
  the quest.
- Watches demand through the `demand()` handles
  [Demand everywhere](/quest/m1/demand-everywhere.md) plumbs.

## Required

- [Demand everywhere](/quest/m1/demand-everywhere.md) - requests and producers at every level watch subscribers through `demand()` alone
