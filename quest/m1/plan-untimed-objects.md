# [S] Plan: untimed peer objects

## Goal

Decide what a frame's timestamp is when a peer sends none, and rewrite this
quest into implementation quests. That covers IETF objects on a track without
TIMESCALE or without a Timestamp property, and lite-01 to lite-04, which have
no timestamp on the wire. The decision must not invent a timeline that
changes with the relay path.

## Plan

Today every receiver fills local arrival time (`Timestamp::from(runtime.now())`
in `rs/moq-net/src/{lite,ietf}/subscriber.rs`, `?? Timestamp.now()` in js/net),
as `drafts/draft-lcurley-moq-timestamp.md` mandates. On lite-05 and later, a
relay then forwards those stamps as if they were real. A failover to another
first hop changes them, so the timeline jumps, and they break the "one clock
per broadcast" property. The maintainer dislikes this.

Settled elsewhere: publishing always requires a timestamp
([Publishing requires a timestamp](/quest/m1/publish-timestamp.md)), and
lite-05 and lite-06 (published) can't encode an absent one.

Candidates:

- Faithful absence (leaning): the model and consumers carry
  `Option<Timestamp>` so a relay forwards "none" as none. lite-07-wip encodes
  it by shifting the FRAME Timestamp Delta and the DATAGRAM Timestamp by one
  (0 = absent); an absent frame doesn't move the delta baseline. A lite-05/06
  encoder writes its send time. Untimed groups are never media-stale; the
  pool's wall-clock expiry still reclaims them, and start resolution picks the
  latest group instead of replaying the cache.
- First-hop arrival, documented: no change, but the failover jump stays.
- 0 as a sentinel: rejected. It collides with a real pts of 0.
- `max_age` on max(wall, pts): the maintainer asked. Rust keeps `max_age` as
  media-time staleness so a congestion stall can't age content out, with the
  pool's expiry as the wall-clock bound (`rs/moq-net/src/model/cache.rs`).

Facts to gather: which IETF implementations send TIMESCALE and Timestamp
(moxygen, the interop matrix), and every model decision that reads a
timestamp (`track.rs` staleness and `GroupExpiry`, `resume.rs` successors,
`group.rs` `poll_timestamp`, which treats "has a timestamp" as "has a frame").

Output: implementation quests, and updates to
[Data consumer timestamps](/quest/m1/data-consumer-timestamps.md), whose
return type depends on this, and to the timestamp and lite drafts.
