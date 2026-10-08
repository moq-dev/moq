# [M] FETCH_OK carries the track's properties

## Goal

Our IETF publisher's FETCH_OK carries the same track properties its
SUBSCRIBE_OK does: max cache duration, timescale, priority, and group order.
A fetch-only reader then learns the track as a subscriber would, and a relay
meets draft 16+'s rule to include all of a track's properties.

## Plan

Found (2026-10-01) in #4647: our publisher's FETCH_OK sends an empty
properties block. Our own subscriber no longer reads it (#4974): it learns a
track from SUBSCRIBE_OK or TRACK_STATUS_OK before any FETCH, and from draft 20
opts out with INCLUDE_PROPERTIES = 0. Other fetch-only readers still need it.

- `rs/moq-net/src/ietf/publisher.rs` sends FETCH_OK with
  `properties: Default::default()`. SUBSCRIBE_OK fills them in the block near
  `publisher.rs:655`. Share that code so the two can't diverge.
- Drafts 14-15 allow the omission: MAX_CACHE_DURATION is a MAY there, and
  FETCH_OK has no properties block. From draft 16, a relay MUST include all
  Extension Headers / Properties associated with a track in FETCH_OK (d16
  §8.6, d22 §7.7). An empty block also misleads the reader: it infers
  Ascending order and priority 128 where SUBSCRIBE_OK says Descending.
- FETCH preserves object properties, Timestamp included. Decided in the
  2026-10-05 audit (maintainer: "FETCH must send stamped objects? It's not
  legal to remove the property."). [#4822](https://github.com/moq-dev/moq/pull/4822) makes the standalone FETCH keep
  each object's Timestamp in the track's units, since a subscribed relay
  treats a timed track's object without one as malformed. What's left here is
  FETCH_OK declaring the track's TIMESCALE, so a fetch-only reader is timed
  exactly when the track is, as the untimed model requires. Today our
  subscriber takes the units only from SUBSCRIBE_OK, and a fetch-only reader
  is untimed. Rejected: omitting timescale from FETCH_OK and leaving
  fetch-only readers untimed.
- Honour INCLUDE_PROPERTIES (0x35) on FETCH from draft 20. It defaults to sending the
  properties; at 0 the block is present but empty. `fetch.rs` decodes it into
  `Fetch::properties_wanted`, which the publisher does not honour yet.
- Test per draft range: FETCH_OK round-trips the properties SUBSCRIBE_OK would
  carry, a standalone FETCH's objects arrive stamped, and
  INCLUDE_PROPERTIES = 0 empties the block.

Public API: none. Wire: FETCH_OK gains its properties on drafts that define
the block. Interop: run `just test interop --all`.
