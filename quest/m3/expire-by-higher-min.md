# [M] A group expires by its own highest timestamp, or a higher group's lowest

## Goal

Timestamp expiry bounds a whole group by its own highest frame timestamp
(inclusive), and any other group, or a hole, by the lowest frame timestamp
this hop knows in any higher group (exclusive). That bound applies whether or
not the higher group is the immediate successor. The latest group never ages.
Arrival age, the wall time since a group's successor arrived, still expires a
superseded group on its own. Rust, `@moq/net`, and the moq-lite draft's
Expiration section change together.

## Plan

Decided 2026-11-20 (maintainer, /quest-plan auditing the subscription rules
before the lite-07 cut): skipping is by timestamp, from a group's maximum, or
from a higher group's minimum while the group isn't whole. Arrival age stays
beside it as the backstop for a publisher whose timestamps stall, or an
abusive one that never advances them, whose stale streams would otherwise
hold connection credit other broadcasts need. ✅ Both clocks (rejected:
timestamps only). ✅ The latest group never ages by the wall clock (rejected:
[moq-dev/moq#9221](https://github.com/moq-dev/moq/pull/9221)'s aging of a
whole group with no successor from an estimated mean frame interval, and
main's `ended_at`, which ages it from GROUP_END's end).

Today `reach` (`rs/moq-net/src/model/track.rs`) and `#reach`
(`js/net/src/track.ts`) use GROUP_END's end, else the immediate successor's
frame 0. The draft says the same ("A group's reach is its own end once it
finished ... otherwise the first frame timestamp of the next group by start").
#9221 was asked to adopt the whole-group half (its own highest timestamp
whenever something newer measures it) and to drop the latest group's
wall-clock aging. Whatever it leaves undone is this quest's.

- A higher group's lowest timestamp bounds a lower group only under the
  strong no-overlap rule, so this waits on it. Until then moq-mux's
  overlapping groups would expire early.
- Use any known frame of any higher group, a tail copy's included: under the
  rule every one of them is above the lower group's frames.
- Find every other place that bounds a group or a hole by a successor's start,
  such as a reader giving up a hole by the max delay, and apply the same bound.

Public API: none expected. Wire: none; the draft's Expiration text changes,
which only lets a publisher expire sooner.

## Required

- [Every frame of a group presents before every frame of a higher group](/quest/m2/groups-never-overlap.md) - the rule that makes a higher group's lowest timestamp an upper bound
