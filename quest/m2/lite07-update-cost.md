# [M] A lite-07 SUBSCRIBE_UPDATE costs its edit on a relay's front and in JS too

## Goal

A SUBSCRIBE_UPDATE that edits k ranges of an n-range subscription costs
O(k log n) on the publisher, in the session's own task as well as under the
lock every subscriber of the track shares, in Rust and JS alike.

## Plan

Left by [#9134](https://github.com/moq-dev/moq/pull/9134), which made the
Rust publisher hand the model the edit by Range ID
(`track::Subscriber::edit`), keyed each subscriber's ranges with a change
log (`model/track/demand.rs` `Prefs`), and made the plain cursor, its
window, the misses, and the shared aggregate move only what an edit touched
(`model/subscription/cover.rs`). `publisher_update` measures it. What
remains:

- Rust, a relay's front (`rs/moq-net/src/model/resume.rs`): `Reader::sync`
  materializes the whole `Subscription` on each change, rebuilds the reader's
  window, and hands every copy a whole `update`, whose own cursor then
  rebuilds. The demand fold (`poll_combined`) materializes each reader's
  preferences too. Replay `Prefs::changes` into the reader's window and pass
  each copy the edit, clamped per copy, through `track::Subscriber::edit`; the
  copy's positional `resolved_ranges` and `Copy::from` become keyed. Add a
  routed case to `publisher_update`.
- Rust, the servable index (`model/track.rs` `Live`): rebuilt from the cache
  arrivals whenever the groups' cover moves, so an edit costs the cache held,
  not the ranges.
- JS: `combineSubscriptions` in `js/net/src/track.ts` rebuilds the union over
  every subscriber on each `Subscriber.update`; give it the incremental union
  `rs/moq-net/src/model/track/demand/ranges.rs` keeps, and mirror the Rust
  edit path in `js/net/src/lite/publisher.ts` and `lite/subscribe.ts`.

The rework is internal-only: no public API changes in `moq-net`, `moq-ffi`, or
`@moq/net`. Keep it that way:

- Rust: the edit entry point stays crate-private; the public `update` stays a
  whole-set replacement and `subscription()` keeps returning ranges in the
  order last given.
- JS: reach the edit through an internal `hooks.*` accessor, as the lite
  publisher already does for the track, and keep the per-producer union
  incremental behind the existing `Subscription` signal. `Subscriber.update`
  and the exported types stay as they are.
- If the work turns up a need for a public change after all, stop and ask:
  the maintainer's condition was that this rework stays internal.
