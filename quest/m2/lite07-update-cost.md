# [M] A lite-07 SUBSCRIBE_UPDATE costs its edit on the publisher's session too

## Goal

A SUBSCRIBE_UPDATE that edits k ranges of an n-range subscription costs
O(k log n) on the publisher, in the session's own task as well as under the
lock every subscriber of the track shares, in Rust and JS alike.

## Plan

Left by the [range union fix](https://github.com/moq-dev/moq/pull/9045), which
made an update that leaves every range live cost nothing for the ranges, and
held the shared lock only for the ranges an edit moves. What remains is linear
in n on the session's own task for any update that edits a range or changes a
preference: n is bounded by one message's ranges, so this is CPU a peer spends
one small message to buy, not unbounded work.

- Rust: `TrackRun::update` (`rs/moq-net/src/lite/publisher.rs`) re-pins every
  range, rebuilds its Range ID positions and `Bounds`, and hands the model a
  whole `Subscription`. The model resolves ranges by position
  (`track.resolved()`), so editing by Range ID means the model's ranges and
  their resolutions are keyed rather than positional
  (`rs/moq-net/src/model/track.rs`, `model/track/demand.rs`). A preference-only
  update still clones the model's ranges.
- JS: `combineSubscriptions` in `js/net/src/track.ts` rebuilds the union over
  every subscriber on each `Subscriber.update`, so a preference-only update
  costs the whole audience there; give it the incremental union
  `rs/moq-net/src/model/track/demand/ranges.rs` keeps.

Recommended: key the model's ranges and their resolutions by an id the
caller assigns, so a publisher passes the edit straight through, and measure
an update stream against a 16k-range subscription before and after.
