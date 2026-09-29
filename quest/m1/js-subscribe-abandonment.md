# [S] Commit JS subscription abandonment without a microtask gap

## Goal

A viewer returning during IETF subscription setup keeps the live track instead
of receiving an abandonment error.

## Plan

The setup path is the same on main, so the fix lands there.

- `waitAbandoned` (in `#runSubscribe`, `js/net/src/ietf/subscriber.ts`)
  checks demand and resolves through `race`; another microtask can attach a
  viewer before the catch awaits `sessionCause` and rejects the request.
  Reproduce that ordering in `js/net/src/ietf/subscriber.test.ts`: "returning
  demand survives a blocked unsubscribe" covers only the established serving
  loop.
- Recheck demand and commit the close in the same synchronous continuation,
  the way the serving loop after SUBSCRIBE_OK does (its `producer.used`
  re-check before `producer.close()`). When demand returns, keep the
  existing setup operation and timeout budget.
- Cover abandonment before SUBSCRIBE_OK, demand returning before the commit,
  and late setup completion. Verify cancellation and alias cleanup still happen
  exactly when owed.

## Related

- [#3455](https://github.com/moq-dev/moq/pull/3455) - the Rust atomic teardown and established JS loop fix
