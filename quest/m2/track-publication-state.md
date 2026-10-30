# [S] Track publication is one state

## Goal

`TrackState`'s `published`, `staged`, and `claimed` booleans
(`rs/moq-net/src/model/track.rs`) become one private enum, so a combination
the lifecycle never reaches, such as staged but unpublished, cannot be
represented. Each transition is one method that sets the whole state. No API
or wire impact.

## Plan

Decided 2026-10-27 (maintainer interview, from the 2026-10-27 refactor audit):
narrow to publication only. A broader split of `TrackState` by invariant was
declined; `info` stays separate because a reverse fetch installs it before
acceptance, and the `arrival` and `evict` indexes stay distinct.

- Today's transitions: `Request::claim` sets `claimed`; `Request::stage` sets
  `published` (through `accept`) and `staged`; `Staged::commit` clears
  `staged`. So the states are pending, claimed, staged, and published, and a
  claimed request can still be staged and published.
- Readers: `poll_info` hides `info` while staged, and the reject check treats
  anything but pending as answered.
- Waits for front eviction, which is rewriting the same file.

## Required

- [Front eviction](/quest/m1/unbounded-groups/front-eviction.md) - lands its `track.rs` rewrite first
