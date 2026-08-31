# [S] Better prioritize ties

## Goal

Implement and verify the behavior tracked in [#699](https://github.com/moq-dev/moq/issues/699)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

The way QUIC libraries provide stream prioritization is extremely limited. You can basically set an integer priority for each stream.

This makes it very difficult to do things like interleave equal priorities. For example, suppose we request audio from Alice and Bob at priority 5.
Ideally, we should prioritize the latest group for both equally, then equally deprioritize older groups.

However the current implementation will break the tie based on the group number, so basically it will prioritize whoever has been live for longer. This is especially broken in the current relay implementation, because every track *should* be requested with the same priority over the backbone. Since they all tie, all data gets prioritized according to group ID.

## Related

- [Transmission order](/quest/m1/ladder/transmit.md) - the other open question
  at this seam: lower renditions should win a tie among equal subscriber
  priorities

## Closes

- [#699](https://github.com/moq-dev/moq/issues/699) - close this issue when the quest finishes
