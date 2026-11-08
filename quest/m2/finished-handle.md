# [L] Finishing a producer returns a Finished handle

## Goal

`finish(self)` consumes a producer and returns a `Finished` handle, so
appending after finish is a compile error instead of a runtime one. The
handle keeps what finishing still allows: aborting, which reports a later
failure and releases the cache, and, where the type supports it today,
inserting below the end, such as a late group under a finished track's end.
It has no append. One shape covers moq-net's `track::Producer` and
`group::Producer` and the media producers and importers, with JS mirrors.

## Plan

Decided 2026-11-07 (maintainer, /quest-complete), a follow-up of
[moq-dev/moq#8210](https://github.com/moq-dev/moq/pull/8210), which made a
failed finish final with explicit state and recommended against
`finish(self)`. The maintainer chose the typestate:

- ✅ **Scope: moq-net and media.** moq-net's `track::Producer` and
  `group::Producer`, plus #8210's producers and importers (the container
  producer, the MKV, fMP4, FLV, and TS importers, the audio and JSON
  producers), and their `js/net` and `js/hang` mirrors. Not chosen: moq-net
  only; media only.
- ✅ **Name: `Finished`**, as `track::Finished`, `group::Finished`, and so
  on.

Today each producer borrows on finish and enforces the rest at runtime.
`track::Producer::finish(&self)` (`rs/moq-net/src/model/track.rs`) sets the
end after `max_sequence`, so appending at or above it fails, while older
groups below it can still arrive. `group::Producer::finish(&mut self)`
(`rs/moq-net/src/model/group.rs`) borrows so that `abort` can still report
a later failure and its cache stays readable. `Finished` makes exactly that
contract the type. Holding a finished importer or producer, which #8210
documents as a use, becomes holding its `Finished`. Decide per type what
`Finished` exposes beyond abort, such as read access to the cache or insert
below the end, without adding anything the producer can't do today.

This breaks every caller that finishes and then keeps using the producer.
List them in the PR and update each package's `MIGRATE.md`.

Public API: breaking in moq-net, moq-mux, moq-audio, moq-json, and their JS
packages. Wire: none.

## Related

- [Track and group end-of-life](/quest/m1/explicit-state/track-end.md) - the outcome enums `Finished` would wrap
- [A failed finish is final](https://github.com/moq-dev/moq/pull/8210) - the explicit state this builds on
