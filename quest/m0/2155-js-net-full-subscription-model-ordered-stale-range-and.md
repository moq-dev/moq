# [L] js/net: full Subscription model (ordered/stale/range) and Subscriber.update()

## Goal

Implement and verify the behavior tracked in [#2155](https://github.com/moq-dev/moq/issues/2155)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

js/net only models `priority` on subscriptions, while Rust has the full `Subscription { priority, ordered, stale, group_start, group_end }` model with `Subscriber::update()`. The lite wire messages already round-trip all the fields (`js/net/src/lite/subscribe.ts`), so this is model/API work, not wire work:

- `Broadcast.Producer.subscribe(name, priority)` / `Broadcast.Consumer.subscribe(name, priority)` take a positional `priority` (`js/net/src/broadcast.ts`); should take a Subscription-shaped options object like `Track.Consumer.subscribe(options?)` already does.
- `Track.Subscriber.updatePriority(priority)` should become `update(subscription)` mirroring Rust; the name bakes in a single knob (`js/net/src/track.ts`).
- `ordered`/`stale`/`group_start`/`group_end` need to be plumbed from the options object into the SUBSCRIBE message and applied on the publisher side.
- Default alignment: the JS `Subscribe` message defaults `ordered ?? false` while `SubscribeUpdate` defaults `ordered ?? true` and Rust's `Subscription::default()` is `ordered: true` (`js/net/src/lite/subscribe.ts:21,103`). Pick one default (Rust's `true`) everywhere.

**Timing note**: the *shape* items (options object instead of positional priority, `update()` instead of `updatePriority()`, the ordered default) are cheapest before the dev->main merge since renames after release are breaking; the full ordered/stale/range plumbing can follow additively.

## Closes

- [#2155](https://github.com/moq-dev/moq/issues/2155) - close this issue when the quest finishes
