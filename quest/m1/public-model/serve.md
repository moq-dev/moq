# [XS] Public JS feed start

## Goal

A custom `@moq/net` publisher learns where the source's feed starts, as the
public Rust `track::Subscriber::poll_start` answers. The built-in lite
publisher uses the same call.

## Plan

The rest of public serving landed: the `with_range`/`withRange` cursor, public
`poll_start`, `poll_drop`, `poll_expired`, `Position::final_sequence`,
`Subscription::{start, end, with_start, with_end}`, and in JS the `Recv` union
from `tryRecvGroup`, `groupChanged`, `Ordered.tryNextGroup`, `finalSequence`,
`Track.Consumer.info({ hold })`, and `Group.Consumer.expired`. A publisher
gives up on a group it is still writing once `expired` settles, or `closed`
(Rust `poll_finished`) fails. The live-edge answers moved to
[track-live](/quest/m1/track-live.md), and `track::Consumer::final_sequence`
to [the deferred quest](/quest/m1/public-model/deferred.md).

Left: the JS `hooks.sourceStart`, which
[#6206](https://github.com/moq-dev/moq/pull/6206) added after this cut so the
lite publisher keeps a descending feed's start. It is the JS counterpart of
`poll_start` (the source's start, raised to the cursor floor, or `"pending"`
until the upstream START arrives); make it a public `Track.Subscriber` call.

## Related

- [Public model](/quest/m1/public-model/README.md) - the epic, its rules, and the inventory method
- [A reader can tell when its track is live](/quest/m1/track-live.md) - owns the publishers' live-edge answers
