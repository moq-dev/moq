# [S] js/net ages cached groups on a cache window, not max_age

## Goal

js/net retains cached groups the way Rust does. A wall-clock idle window
bounds memory, and `max_age` is only the media-time staleness budget, so a
congestion stall can't age content out of a JS publisher's cache.

## Plan

`#prune` (`js/net/src/track.ts`) evicts a group after `info.maxAge` of
wall-clock idleness, measured on the group's `activity`. Rust keeps those
apart: `track::Info::max_age` is measured in media timestamps, and the cache
pool's `expiry` is the separate wall-clock bound
(`rs/moq-net/src/model/cache.rs`). Give js/net a cache window of its own for
idle eviction, as Rust's pool has, and leave `maxAge` to `#drift` /
`#isStale`. Add a test with mocked time where a stalled track keeps its groups
past `maxAge`.

[Generated @moq/net](/quest/m1/rs2ts/README.md) will replace this code with
Rust's model; the maintainer chose to fix it by hand first.

Public API: none unless the window is configurable (keep it private). Wire:
none.

## Related

- [Generated @moq/net](/quest/m1/rs2ts/README.md) - retires js/net's hand-written model
