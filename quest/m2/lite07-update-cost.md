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

The rework is internal-only: no public API changes in `moq-net`, `moq-ffi`, or
`@moq/net` (checked when the range union fix landed, the condition for leaving
this to m2). Keep it that way:

- Rust: add a crate-private edit entry point on `track::Subscriber` (retired
  ids, added `(id, Range)` pairs, and the preferences) beside the public
  `update(Subscription)`. The per-subscriber ranges and the window's
  resolutions become keyed; `Window` is `pub(in crate::model)`, and
  `Resolved`, `resolved()`, and `Preferences` are crate-private already. The
  public `update` stays a whole-set replacement, costing what its caller
  passes, and `subscription()` keeps returning ranges in the order last given.
  `Subscription`, `Range`, and the aggregate getters keep their shapes.
- JS: reach the edit through an internal `hooks.*` accessor, as the lite
  publisher already does for the track, and keep the per-producer union
  incremental behind the existing `Subscription` signal. `Subscriber.update`
  and the exported types stay as they are.
- If the work turns up a need for a public change after all, stop and ask:
  the maintainer's condition was that this rework stays internal.
