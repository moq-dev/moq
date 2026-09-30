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
- `start_floor_suppresses_late_lower_arrivals` stays: it sets an explicit floor
  of 7, so group 5 is still below it.

If `drafts/draft-lcurley-moq-lite.md` describes `SUBSCRIBE_START` as an
implicit drop below the start regardless of the floor, update it in the same
PR.

Test: port the reporter's deterministic repro
(`rs/moq-net/tests/late_lower_group.rs` on kidq330's fork, paused time and the
mock transport) for lite-05, lite-06, and lite-07-wip, direct and through a
relay, with the in-process and moq-transport controls.

## Closes

- [#4595](https://github.com/moq-dev/moq/issues/4595) - moq-lite: a group created below the first served group is silently dropped

## Related

- [SUBSCRIBE_DROP](/quest/m1/subscribe-drop.md) - once publishers name every undelivered group, a group dropped under the None floor can be named instead of vanishing
