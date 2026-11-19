# [M] DVR rewind

## Goal

A viewer seeks back by time and returns to live playback. A seek is a
`Time(pts)` range, answered by the live broadcast's cache while the content is
young enough, and otherwise by an archive broadcast that a host serves through
`archive::Consumer` at a path the application gives the player.

## Plan

Retargeted 2026-10-17 with the [archive design](/doc/concept/archives.md):
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

- Decided 2026-11-15 (/quest-import of
  [moq-dev/moq#1971](https://github.com/moq-dev/moq/pull/1971), a deep
  replay window up to live, such as 72 hours): this quest owns it, with no
  quest of its own. Rejected: a synchronous `GroupSource` callback on the
  live track, since a callback forces history reads to block (bad over S3).
  An application with its own store answers evicted groups through
  `track::Producer::requests()`, whose `Fill` it serves asynchronously, or
  records with `moq-archive`. Rejected: serving the deep window on the live
  track itself, since its `max_age` marks those groups `Old`; the archive
  stays at its own path.

Test seeks within the cache, past it into the archive, across a pruned start,
over missing groups, and return to live.
