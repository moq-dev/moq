# [S] moq-transport upstream updates

## Goal

A relay's moq-transport upstream subscription follows its downstream
subscribers: when their aggregate subscriber priority changes, the relay sends
SUBSCRIBE_UPDATE (drafts 14 and 15) or REQUEST_UPDATE (draft-16 on) with the
new priority, as lite-07 already does. Today the IETF subscriber reads the
aggregate once, at SUBSCRIBE (`rs/moq-net/src/ietf/subscriber.rs`,
`js/net/src/ietf/subscriber.ts`), and never updates the request.

Group order is not part of this. The upstream order stays whatever SUBSCRIBE
asked for (decided 2026-10-13 in #5400: updates carry priority only, with no
re-subscribe and no forced Descending upstream).

## Plan

Researched 2026-10-13 against the published drafts 14 to 22: no moq-transport
draft lets a subscriber change a subscription's group order.

- Every draft says the group order of an existing subscription cannot be
  changed (§7.1 in 14 to 20, §5.1.1 in 21 and 22).
- Draft-14's SUBSCRIBE_UPDATE has no Group Order field. From draft-15 on,
  GROUP_ORDER's list of allowed messages leaves out the update message, and
  draft-22 spells out per request type what a REQUEST_UPDATE may carry.
- Sending it anyway is ignored on 14 and 15, ambiguous on 16, and a
  PROTOCOL_VIOLATION from draft-17 on. Draft-20 to 22 allow GROUP_ORDER inside
  FILL_PARAMETERS, but that orders a new fill, not the subscription.
- The only way to change the order is to UNSUBSCRIBE and SUBSCRIBE again with
  a fill or FETCH for the gap. That is not planned: it costs a round trip and
  a gap to change a transmission preference on one leg, and the relay already
  serves each downstream subscriber in its own order.

What an update can carry, and this quest sends:

- Subscriber priority, on every draft: a fixed field on draft-14, the
  SUBSCRIBER_PRIORITY parameter after.
- Draft-14's SUBSCRIBE_UPDATE also requires Start Location and End Group, so a
  priority update repeats the subscription's current bounds rather than
  zeroes, which a conforming peer would read as narrowing to group 0.
- From draft-15 on, a priority update omits the subscription filter. Today
  `SubscribeUpdate::encode_msg` (`rs/moq-net/src/ietf/subscribe.rs`) always
  writes `Filter::NextObject`, which would reset the filter.
- Forward stays 1. Pausing is not part of this.
- The subscription filter is updatable from draft-15 on. Narrowing upstream
  ranges belongs to [moq-transport ranges](/quest/m1/subscribe-ranges/ietf.md),
  which can reuse this update path.

Implementation: the IETF subscriber watches the aggregate the way
`lite/subscriber.rs` does (`buffer_update`), sending one update per change
and coalescing changes while one is in flight. The publisher side already
applies a priority update to open group streams
(`subscription_update_reprioritizes_open_group_streams`), and so does the JS
publisher on every draft. Test on mock time,
in Rust and JS, directly and through a relay: a second downstream subscriber
at a higher priority raises the upstream priority, and leaving lowers it again.

## Related

- [Scope and sign track priority](/quest/m1/track-priority-scope.md) - may stop forwarding downstream priority upstream on relay-to-relay links, which changes what this sends
