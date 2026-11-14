# [L] Refresh a track by interval, and on request

## Goal

A publisher can declare in TRACK_INFO a refresh interval: a live join asks
for a new group only once the newest group arrived at least that long ago.
A reader can also ask for one with a public `refresh()` on the track
subscriber, in Rust and `@moq/net`, judged by the same interval. Every hop
coalesces requests and judges them against its own cache, so the
publisher's interval caps how often new groups are made, by design rather
than by a per-hop rate limiter.

The interval is a duration, encoded and read like `max_age`, so it means the
same on a timed and a scale-less track. A group's age is measured on the
judging node's own monotonic clock from when that node first received (or, at
the publisher, created) the group, as arrival-age expiry already measures it.
It never compares media timestamps, which carry no wall-clock meaning and, on
a scale-less track, no unit, and it adds no local clock to the model.

A track of one-frame groups, such as a [thumbnail](/quest/m4/hang-thumbnail.md),
then produces a fresh group when a reader arrives or asks, and never more
often than its interval, however many readers fan out behind the relays.
A track with no group yet needs no interval: its first reader asks for the
first group at once, as the first consumer of an empty track does today
(`wanted()` is `Some(0)`), and the interval applies from that group on.

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
  field. The request barrier above rides the update numbers, not that start
  time, so it needs no clock shared between hops.
- `rs/moq-video/src/encode/producer.rs` forces a keyframe on a join; it
  keeps working on the byte ratio.

Recommendations for the PR:

- Whether the interval sits beside the byte ratio or replaces `Refresh` with
  a typed choice. A track may want both (video wants a keyframe when the GOP
  is long, and not more than once a second), so recommend both, either one
  firing within the interval.
- `refresh()` has a request barrier, and it must be causal at the publisher:
  a group already in flight when the request reached a relay would pass an
  arrival check while showing the state before the request. So a group
  answers a request only if the publisher created it after receiving that
  request. Recommend reusing SUBSCRIBE_UPDATE's update number: each hop
  forwards a request as a new update after it receives it (coalescing
  requests that arrive between two forwards), the publisher tags each group
  it creates with the highest update it had seen, and `refresh()` resolves
  only with a group tagged at or past the update its request rode. The PR
  settles the field's place in the draft and measures its cost; if it costs
  more wire than it earns, weaken the guarantee to "a group that reached the
  node after the request", state that post-action freshness is then best
  effort, and change the thumbnail's promise to match. The interval may
  delay the new group, never substitute an older one. Rust and `@moq/net`
  share this contract.
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
