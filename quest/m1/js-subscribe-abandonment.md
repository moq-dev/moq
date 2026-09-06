# [S] Commit JS subscription abandonment without a microtask gap

## Goal

A viewer returning during IETF subscription setup keeps the live track instead
of receiving an abandonment error.

## Plan

- In `js/net/src/ietf/subscriber.ts`, `waitAbandoned` checks demand before
  resolving through `Promise.race`. Another microtask can attach a viewer before
  the outer catch closes the producer. Reproduce that ordering in a subscriber
  regression test.
- Recheck demand and commit the close in the same synchronous continuation.
  When demand returns, retain the existing setup operation and timeout budget.
- Cover abandonment before SUBSCRIBE_OK, demand returning before the commit,
  and late setup completion. Verify cancellation and alias cleanup still happen
  exactly when owed.

## Related

- [#3455](https://github.com/moq-dev/moq/pull/3455) - the Rust atomic teardown and established JS loop fix
