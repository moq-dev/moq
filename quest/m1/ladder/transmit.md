# [M] Transmission order

## Goal

Lower renditions win the publisher-side tie-break among subscriptions at equal
subscriber priority, so transport shedding drops the top of the ladder first.

## Plan

The allocator divides by the publisher's `track::Info::priority`, but the
local send queue ranks by each subscription's own priority, so today a
congested uplink sheds every rung alike. That is what the
`BitrateUnsupported` fallback leans on when it cannot reclaim encoder work:
without it, an unsupported encoder degrades the whole ladder equally instead
of protecting the bottom.

Subscriber priority stays the primary authority. Rendition order only breaks
ties among subscriptions the subscriber ranked equally, so a subscriber
asking for a higher rendition ahead of a lower one still gets what it asked
for.

Cover equal subscriber priority, conflicting subscriber priority where the
subscriber's order must win, and a custom ladder order.

## Related

- [Hierarchical stream scheduling](/quest/m2/quic/scheduler.md) - supplies the
  fair subscription buckets beneath this rendition policy
