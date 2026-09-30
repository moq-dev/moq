# [S] moq-lite delivers a late group above the subscriber's floor

## Goal

A moq-lite subscriber with an explicit floor receives every group at or above
that floor that its `max_age` still considers fresh, whatever order the
publisher created them in, as moq-transport already does. Today the lite
publisher silently drops a group created below the first group it served: the
publisher's `create_group` and `write_frame` succeed, and the subscriber sees a
clean end without it.

## Plan

`TrackRun::start` (`rs/moq-net/src/lite/publisher.rs`) sends `SUBSCRIBE_START`
for the first served group and then calls `raise_start_to(start)`, which
suppresses every lower group regardless of the subscription's floor and
budget. #4387 added it to resolve a relayed subscription's start from its
source; keep that fix (see the note in
[Track tail interop](/quest/m1/track-tail-interop.md)) while honoring an
explicit floor.

Decided:

- An explicit floor delivers late lower groups within `max_age`, matching
  moq-transport, draft-ietf-moq-transport ("subscriptions without a filter pass
  all Objects"), and moq-mux's floor handling (#3258).
- `start: None` joins where the publisher starts: the first served group
  becomes the floor, so a group created below it is dropped by design. This is
  what moq-mux's `container::Consumer` and the IETF publisher already assume.
  Fix the `Subscription::start` docs in moq-net (which say `None` equals a
  floor of group 0) and the matching js/net docs.
- `raise_start_to` applies only when the subscription has no explicit floor
  (`start.is_none()`). With an explicit floor, `SUBSCRIBE_START` still reports
  the first served group, but nothing below it is suppressed.
- Aggregation follows: `Subscription::poll_combined`
  (`rs/moq-net/src/model/subscription.rs`) lets any `None` clear another
  subscriber's explicit floor, so a relay forwards "join at the start" upstream
  and the explicit-floor subscriber still loses group 0. An explicit floor
  survives mixing with `None`; the loosest explicit floor wins. Each
  subscriber's own cursor still filters what it sees.
- `start_floor_suppresses_late_lower_arrivals` stays as the `None` case: it
  subscribes with `None`, receives group 7, raises the start to 7 as
  `SUBSCRIBE_START` does, and group 5 is still suppressed. Add its explicit-floor
  twin, which delivers group 5.

If `drafts/draft-lcurley-moq-lite.md` describes `SUBSCRIBE_START` as an
implicit drop below the start regardless of the floor, update it in the same
PR.

Test: port the reporter's deterministic repro
(`rs/moq-net/tests/late_lower_group.rs` on kidq330's fork, paused time and the
mock transport) for lite-05, lite-06, and lite-07-wip, direct and through a
relay, with the in-process and moq-transport controls. Add a mixed case through
a relay: a `None` subscriber already receiving group 1, then one with a floor
of group 0, and a fresh group 0 reaches only the second.

## Closes

- [#4595](https://github.com/moq-dev/moq/issues/4595) - moq-lite: a group created below the first served group is silently dropped

## Related

- [SUBSCRIBE_DROP](/quest/m1/subscribe-drop.md) - once publishers name every undelivered group, a group dropped under the None floor can be named instead of vanishing
