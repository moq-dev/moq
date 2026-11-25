# [M] A subscriber reports where its delivery starts

## Goal

The Rust and JS lite publishers name SUBSCRIBE_START from one answer the track subscriber gives: the lowest group its read will hand out, given the first group out and the frame asked. Today three places rebuild that answer from parts, and each fix has to land in all three. Behavior, wire, and public API stay as they are.

## Plan

Recommended by the second reviewer (Claude Opus 5.5) on [moq-dev/moq#9529](https://github.com/moq-dev/moq/pull/9529), which made an arrival read serve held groups newest first. Once the first group out is no longer the lowest, the publisher has to work out where delivery starts. It does that in three places:

- Rust `TrackRun::start` in `rs/moq-net/src/lite/publisher.rs` walks `track::Subscriber::lowest_unread` and checks `servable` against the frames asked. A second loop then steps past runs given up and held groups it can't serve.
- JS `lite/publisher.ts` does the same with `hooks.lowestUnread`, `hooks.heldAt`, and its `skipped` runs.
- The front (`resume::Subscriber::lowest_unread` in Rust, `Reader.lowestUnread`/`heldAt` in JS) merges the copies' answers, picking the most servable copy on a tie.

Recommendations (no open decisions):

- Add one crate-internal method on `track::Subscriber`, for example `delivery_start(first: u64, frame: u64) -> u64`. It returns the lowest group at or below `first` that the read will still hand out from `frame`, past the runs it gave up. A front answers it across its copies. Mirror it as one hook in JS.
- Delete `lowest_unread`, `servable`, and `heldAt`, and the publisher's two loops. `poll_start`, the source's feed start, stays separate. The publisher still takes the lower of the two, because that start comes from the source, not from what the reader holds.
- Keep every existing `lowest_unread`, `servable`, and START test as the regression suite, with Rust/JS twins asserting the same starts. Add a differential test that checks the new answer against the old reconstruction over random held, given-up, and headless groups before deleting the old code.

Public API: none. Wire: none.

## Related

- [A front's ordered reader reports a drop that lands with the group above it](/quest/m1/front-drop-with-next-group.md) - touches the same front reader
