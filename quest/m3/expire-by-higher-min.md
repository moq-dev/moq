# [M] A group that isn't whole expires by a higher group's lowest timestamp

## Goal

Timestamp expiry bounds a whole group by its End (GROUP_END), and any other
group, or a hole, by the lowest frame timestamp this hop knows in any higher
group. That bound holds whether or not the higher group is the immediate
successor. The latest group never ages. Arrival age, the wall time since a
group's successor arrived, still expires a superseded group on its own. Rust,
`@moq/net`, and the moq-lite draft's Expiration section change together.

## Plan

Decided 2026-11-20 (maintainer, /quest-plan auditing the subscription rules
before the lite-07 cut): skipping is by timestamp, from a whole group's end,
or from a higher group's minimum while the group isn't whole. Arrival age
stays beside it as the backstop for a publisher whose timestamps stall, or an
abusive one that never advances them, whose stale streams would otherwise
hold connection credit other broadcasts need.

- ✅ Both clocks (rejected: timestamps only).
- ✅ The latest group never ages by the wall clock (rejected: main's
  `ended_at`, which ages a whole group with no successor from its arrival and
  span; and closed [moq-dev/moq#9221](https://github.com/moq-dev/moq/pull/9221)'s
  estimate of a mean frame interval).

Group times ([moq-dev/moq#9263](https://github.com/moq-dev/moq/pull/9263)) gave a whole group its
End and recommends bounding any other group by its successor's start, as
`reach` (`rs/moq-net/src/model/track.rs`) and `#reach` (`js/net/src/track.ts`)
do today. This quest tightens the second half once producers keep the strong
rule.

- A higher group's lowest timestamp bounds a lower group only under the strong
  no-overlap rule, so this waits on it. Until then moq-mux's overlapping groups
  would expire early. With open-GOP leading pictures, that lowest frame sits
  below the higher group's start.
- Use any known frame of any higher group, a tail copy's included. Under the
  rule, every one of them is above the lower group's frames.
- Drop `ended_at`'s wall-clock aging of the latest group (JS `#endedAt`), if
  group times has not already.
- Find every other place that bounds a group or a hole by a successor's start,
  such as a reader giving up a hole by the max delay, and apply the same bound.

Public API: none expected. Wire: none. The draft's Expiration text changes,
which only lets a publisher expire sooner.

## Required

- [Every frame of a group presents before every frame of a higher group](/quest/m2/groups-never-overlap.md) - first-party producers keep the rule that makes a higher group's lowest timestamp an upper bound
