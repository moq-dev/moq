# [L] Refresh a track by interval, and on request

## Goal

A publisher can declare in TRACK_INFO a refresh interval: a live join asks
for a new group only once the newest group started at least that long ago.
A reader can also ask for one with a public `refresh()` on the track
subscriber, in Rust and `@moq/net`, judged by the same interval. Every hop
coalesces requests and judges them against its own cache, so the
publisher's interval caps how often new groups are made, by design rather
than by a per-hop rate limiter.

A track of one-frame groups, such as a [thumbnail](/quest/m4/hang-thumbnail.md),
then produces a fresh group when a reader arrives or asks, and never more
often than its interval, however many readers fan out behind the relays.

## Plan

Planned 2026-11-13 (maintainer, /quest-plan of a remote desktop for
kixel.ai). The consumer is Kixel's desktop host, which serves an agent's
screenshot from a lossless thumbnail and wants dashboard previews from the
same track.

Decided:

- ✅ moq m2, in lite-07 (rejected: m4 and lite-08). Its consumer is later
  work, which would put it in m4, but it changes lite-07-wip, and missing
  that freeze costs a whole wire version.
- ✅ An interval in TRACK_INFO plus a public `refresh()` (rejected: changing
  only the byte-ratio check; a per-hop rate limiter). The interval bounds
  the request rate by what the publisher declares.

Facts found while planning:

- `Refresh` is a byte ratio today. `over_refresh`
  (`rs/moq-net/src/model/group.rs`) tests `cache - first > first * ratio`,
  so a complete one-frame group never refreshes at any ratio.
- `Subscriber::join` is `pub(crate)` in `rs/moq-net/src/model/track.rs`.
  The only trigger is a live-edge subscribe, and no reader can ask for a
  group.
- SUBSCRIBE_UPDATE's `Wanted Group` already carries a relay's coalesced
  demand upstream, and [time addressing](/quest/m2/time-addressing/lite07.md)
  turns it into a start time. Build on that rather than adding a second
  field.
- `rs/moq-video/src/encode/producer.rs` forces a keyframe on a join; it
  keeps working on the byte ratio.

Recommendations for the PR:

- Whether the interval sits beside the byte ratio or replaces `Refresh` with
  a typed choice. A track may want both (video wants a keyframe when the GOP
  is long, and not more than once a second), so recommend both, either one
  firing within the interval.
- `refresh()` asks for a group starting after the newest one the reader
  holds, and resolves when one arrives, which the interval may delay. It
  never resolves with a group no newer than the one the reader held.
- Update the lite draft (TRACK_INFO, SUBSCRIBE_UPDATE, and the changelog),
  `doc/concept/moq-lite.md`, and the matching JS. Run `just test interop
  --all`.

Tests: a relay chain where many readers join and call `refresh()` on a
one-frame track produces one group per interval at the publisher, with time
mocked.

Public API: additive (`refresh()`, the interval on `Info`). Wire:
lite-07-wip TRACK_INFO and SUBSCRIBE_UPDATE.

## Required

- [The lite-07 wire addresses groups by start](/quest/m2/time-addressing/lite07.md) - `Wanted Group` becomes the start time this requests by

## Related

- [Hang thumbnails](/quest/m4/hang-thumbnail.md) - the one-frame track this refreshes
