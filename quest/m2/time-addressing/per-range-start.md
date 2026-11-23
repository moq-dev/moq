# [L] A lite-07 publisher answers each range from its own start, on every track

## Goal

On lite-07, every Range's SUBSCRIBE_START waits only on that range, on an
origin's track as on a relay's copy, in Rust (`lite/publisher.rs` `TrackRun`)
and `@moq/net` (`js/net/src/lite/publisher.ts`). Nothing subscription-wide
decides where a range starts or holds its answer back. The only
subscription-wide state left is whether the first SUBSCRIBE_START went out,
which holds the drops. lite-06 and earlier keep their one start.

## Plan

Found in [moq-dev/moq#9444](https://github.com/moq-dev/moq/pull/9444), which
answers each range on its own only on a relay's copy that places ranges as
its upstream would (`TrackRun::per_range`, `places_any`; JS `perRange`,
`placesAny`). The rest still runs through one start: `Start::Unresolved`
holds the first group until `track::Subscriber::poll_start` resolves the
whole feed, and `Start::Resolved(from)` clamps later answers.

Three Codex review rounds on that PR each found the same coupling, one range's
answer waiting on subscription-wide state:

- Round 1, high: a warm relay held a placed time range behind its live join,
  which the copy's whole feed start waited on.
- Round 2, medium: a copy-side fix that skipped only unanswered joins left a
  placed range held behind older history its upstream still placed.
- Round 3, medium: ranges a SUBSCRIBE_UPDATE added before the first
  SUBSCRIBE_START were left out of the early check, so a placed one waited on
  an initial one.
- While fixing round 3, I found that the early path answered a range reaching
  back to the oldest group at the cursor's floor, before the copy knew where
  its feed starts.

Left coupled after #9444:

- An origin's track, or a copy fed over a wire that reports no range's start,
  holds every range's answer until its whole feed start resolves.
- A range reaching back to the oldest group on a relay's copy waits for the
  copy's whole feed start. The window places it at group 0 (`Resolved`
  `floored: false`), and only the copy's single `FeedStart` says where it
  really begins. Each upstream range's answer would need recording on its
  own.
- `report_started` still raises a range on a track that isn't per-range to
  the delivery start, and held drops below that start stay untold.

Shape: each unanswered range carries its own placement, the first
SUBSCRIBE_START releases the held drops, and the publisher keeps no delivery
start beyond the cursor's floor. The change touches how drops wait for the
first START, so rerun `/verify-subscribe` (`SubscribeAccounting.tla`) and
extend the model if a range's start moves an invariant. Remove the
per-range-copy special case once every track answers per range.

## Related

- [Time anchors by group spans](/quest/m2/time-addressing/span-time-anchors.md) - how a time start resolves to a group, which each range's placement uses
