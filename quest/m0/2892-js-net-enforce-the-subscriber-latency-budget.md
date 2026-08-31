# [M] js/net: enforce the subscriber latency budget

## Goal

Implement and verify the behavior tracked in [#2892](https://github.com/moq-dev/moq/issues/2892)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

\#2890 makes `Subscription::latency` actually enforced in `rs/moq-net`, where it had been metadata that was forwarded on the wire and never acted on. `js/net` still has the same gap.

`js/net/src/track.ts`'s `Subscriber.recvGroup`, `nextGroup`, and `readFrameSequence` hand back buffered groups without consulting `Subscription.latencyMax`, so a browser subscriber replays a backlog it declared it did not want. That is visible in practice whenever another subscriber widens the publisher's aggregate budget, since the aggregate is what reaches the publisher and the per-subscriber semantics only exist locally.

What the Rust side settled on, for a mirror to match:

- Drift is measured in **presentation time**: a group's first frame timestamp against the highest-*sequence* group above it that carries one. Both are stamped once, when the group's first frame is created, so a backlog delivered as a burst still reads as its true age.
- The anchor must sit strictly **above** the candidate. Backfill and the tail of a rewound timeline can carry a high timestamp on a low sequence without being a live edge.
- A group that has presented nothing is never stale, and neither is one with nothing newer stamped above it. Retention bounds those instead.
- The budget is clamped to the publisher's `latencyMax` retention window, since waiting longer than a group is kept cannot produce it.
- The anchor is bounded by what the subscriber could actually be handed (its read cursor's cap), **not** by the end it requested on the wire, which is a preference that does not filter the handle.
- `start` is a filter, not an exemption: a subscriber that wants history raises its budget. A one-shot fetch stays exempt.

Expect the same fallout the Rust side had: tests that write a batch of groups and then read them all back are asserting more than the default `REAL_TIME` budget promises, and each needs to declare the tolerance it was implicitly relying on.

Same shape as the mirrors in #2772 and #2775.

## Closes

- [#2892](https://github.com/moq-dev/moq/issues/2892) - close this issue when the quest finishes
