# [S] Plan: untimed objects

## Goal

Rewrite this quest into implementation quests that carry an absent timestamp
faithfully, from publisher to consumer, through every relay. That covers
untimed publishes, IETF objects on a track without TIMESCALE or without a
Timestamp property, and lite-01 to lite-04, which have no timestamp on the
wire. Nothing may invent a timeline that changes with the relay path.

## Plan

Today every receiver fills local arrival time (`Timestamp::from(runtime.now())`
in `rs/moq-net/src/{lite,ietf}/subscriber.rs`, `?? Timestamp.now()` in js/net),
as `drafts/draft-lcurley-moq-timestamp.md` mandates. On lite-05 and later, a
relay then forwards those stamps as if they were real. A failover to another
first hop changes them, so the timeline jumps, and they break the "one clock
per broadcast" property. The maintainer dislikes this.

Decided (2026-10-01): faithful absence, on both the publish and the subscribe
side. The model and every consumer carry `Option<Timestamp>`, a relay forwards
"none" as none, and no layer substitutes arrival or wall time.
[Publishing never invents a timestamp](/quest/m1/publish-timestamp.md) stops
the producers filling in now. Untimed groups are never media-stale; the
pool's wall-clock expiry still reclaims them, and start resolution picks the
latest group instead of replaying the cache.

- lite-07-wip encodes absence by shifting the FRAME Timestamp Delta and the
  DATAGRAM Timestamp by one (0 = absent); an absent frame doesn't move the
  delta baseline.
- lite-05 and lite-06 (published) can't encode absence, so their encoder
  writes its send time, documented in the lite draft as a downgrade for old
  peers.
- Rejected: first-hop arrival (the failover jump stays), 0 as a sentinel
  (collides with a real pts of 0), and `max_age` on max(wall, pts) (Rust keeps
  `max_age` as media-time staleness so a congestion stall can't age content
  out, with the pool's expiry as the wall-clock bound,
  `rs/moq-net/src/model/cache.rs`).

What remains is mapping the fallout into implementation quests.

Facts to gather: which IETF implementations send TIMESCALE and Timestamp
(moxygen, the interop matrix), and every model decision that reads a
timestamp (`track.rs` staleness and `GroupExpiry`, `resume.rs` successors,
`group.rs` `poll_timestamp`, which treats "has a timestamp" as "has a frame").

Output: implementation quests (the model and wire carrying `Option`, lite-07
encoding, and the timestamp and lite drafts), and their places in the
`Required` lists of the publish and consumer timestamp quests.
