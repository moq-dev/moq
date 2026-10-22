# [S] Public group abandonment and JS feed start

## Goal

A custom `moq-net` publisher waiting on its transport, for stream credit or to
drain, learns through a public call when to give up on a group: the budget
expired it, or its copy was aborted before the publisher read it all and no
route will continue it. A custom `@moq/net` publisher learns where the
source's feed starts, as the public Rust `poll_start` answers. The built-in
lite and IETF publishers use the same calls.

## Plan

The rest of public serving landed: the `with_range`/`withRange` cursor, public
`poll_start`, `poll_drop`, `poll_expired`, `Position::final_sequence`,
`Subscription::{start, end, with_start, with_end}`, and in JS the `Recv` union
from `tryRecvGroup`, `groupChanged`, `Ordered.tryNextGroup`, `finalSequence`,
`Track.Consumer.info({ hold })`, and `Group.Consumer.expired`. The live-edge
answers moved to [track-live](/quest/m1/track-live.md), and
`track::Consumer::final_sequence` to
[the deferred quest](/quest/m1/public-model/deferred.md).

Left: the crate-private `group::Consumer::poll_abandoned(waiter, held)`,
which [#6140](https://github.com/moq-dev/moq/pull/6140) added so a group
parked on stream credit gives up. It answers `poll_expired` plus the aborted
copy. **Decision** on the public shape:

- Make `poll_abandoned` public and `poll_expired` private, dropping the
  `held` flag the way `poll_expired` did (`poll_open` passes `false` today, so
  an empty finished group parked on credit would then expire). Recommended:
  one call answers "stop writing this group".
- Keep both public, with `held` exposed. A bool parameter callers must reason
  about.
- Keep `poll_abandoned` private: a custom publisher reads the abort on its
  next frame read instead, but holds the credit it waited for until then.

Also left: the JS `hooks.sourceStart`, which
[#6206](https://github.com/moq-dev/moq/pull/6206) added after this cut so the
lite publisher keeps a descending feed's start. It is the JS counterpart of
`poll_start` (the source's start, raised to the cursor floor, or `"pending"`
until the upstream START arrives); make it a public `Track.Subscriber` call.

JS has the same gap: [#6214](https://github.com/moq-dev/moq/pull/6214) races a
parked group open against the private `hooks.guardGroup`, since
`Group.Consumer.expired` covers the budget but not an evicted copy. Settle its
public shape with `poll_abandoned`'s, as an `abandoned` promise beside
`expired` or by widening `expired` to every terminal give-up.

## Related

- [Public model](/quest/m1/public-model/README.md) - the epic, its rules, and the inventory method
- [A reader can tell when its track is live](/quest/m1/track-live.md) - owns the publishers' live-edge answers
