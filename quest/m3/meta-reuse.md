# [L] Serve metadata readers from the payload copy on lite-07 hops

## Goal

A relay that already holds a track's payload copy serves that track's
metadata readers from it on lite-07 hops too, instead of opening a second
upstream subscription in metadata mode. Under mixed demand each hop then
carries the track's frame headers once instead of twice, and the relay holds
one upstream subscription per track instead of two.

## Plan

Today ([moq-dev/moq#9758](https://github.com/moq-dev/moq/pull/9758)) a relay
keeps a separate metadata copy per track on lite-07, filled by its own
metadata SUBSCRIBE upstream, even while it holds a payload copy. Over an
older hop, which cannot carry the mode, metadata readers already ride the
track's one payload subscription and copy, and a relay never opens a second
payload subscription for them.

Decided 2026-11-27 (maintainer, on [moq-dev/moq#9758](https://github.com/moq-dev/moq/pull/9758)): ✅ ship the separate lite-07 copies
first, and plan reuse here (rejected: building the splice in #9758).

Cost being removed: while a track has both payload and metadata readers
behind a lite-07 hop, every frame header crosses that hop twice: once inside
the payload stream and once as a metadata FRAME (a few bytes per frame). The
relay also holds and drives a second upstream subscription and a second
cache entry for the track. With payload readers alone, or metadata readers
alone, nothing is duplicated. Measure the cost before building: header bytes
and wakes per relay hop, swept over tracks, readers, and relay depth, under
the mixed demand a route-health standby or `moq-hls` index creates, to
confirm the duplicate headers are worth a splice.

What makes it hard: switching a metadata reader between the payload copy and
a metadata subscription is a splice across copies of different modes, per
range. A reader that is on the payload copy when the last payload reader
leaves has to continue from a metadata subscription at exactly the next
group, with no gap or repeat. A reader joining while the payload copy covers
only part of its range is served from both. [moq-dev/moq#8882](https://github.com/moq-dev/moq/pull/8882)
found this cross-copy splice the expensive part of metadata serving and
dropped mode switching in place; the 2026-11-16 decision in
[route-health](/quest/m3/route-health/README.md) keeps the two subscriptions
separate for that reason. Reuse the logical-track splice the origin front
already does across route switches (`rs/moq-net/src/model/resume.rs`,
checked by `/verify-resume`) rather than a second machine, and extend that
model to cover a switch between modes.

Done when, on lite-07 in both Rust and JS, a relay with payload and metadata
readers of one track holds one upstream subscription. When the last payload
reader leaves, metadata readers continue without a gap through a metadata
subscription, and the payload subscription closes. Tests and the sweep show
headers crossing each hop once. The draft's metadata section loses its MAY
for this case, so the behavior is stated.

## Related

- [Route health](/quest/m3/route-health/README.md) - the metadata standby that creates mixed demand
