# [M] DVR rewind

## Goal

A viewer seeks back by time and returns to live playback. A seek is a
`Time(pts)` range, answered by the live broadcast's cache while the content is
young enough, and otherwise by an archive broadcast that a host serves through
`archive::Consumer` at a path the application gives the player.

## Plan

Retargeted 2026-10-17 with the [archive line](/quest/m1/archive/README.md):
seeking uses time ranges, not timelines, and archives are found by the host,
not through a catalog pointer. The 09-29 decisions that a recording is the
broadcast at the source path, reached by falling through a live
announcement, are superseded: moq-lite routing never falls through to a prefix
route, so the archive lives at its own path.

- Within the publisher's max age, the live broadcast's cache answers the seek.
  Past it, the player subscribes to the archive broadcast with the same range
  and splices back to the live broadcast at its live edge, without duplicated
  or rewound playback. Missing groups remain ordinary gaps.
- How the player learns the archive path is the application's choice; the
  player API takes it as input.
- Decided 2026-10-17: the player surface is `@moq/watch`. Rejected: a Rust
  player first, which no consumer needs yet.

Test seeks within the cache, past it into the archive, across a pruned start,
over missing groups, and return to live.

## Required

- [Subscribe ranges](/quest/m1/subscribe-ranges/README.md) - the `Time(pts)` range a seek sends
