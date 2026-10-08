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

- `run_fetch_stream` (`rs/moq-net/src/ietf/publisher.rs`) sends FETCH_OK
  with `properties: Default::default()`. `run_subscribe_stream` fills them in
  its `ietf::SubscribeOk` encode. Share that code so the two can't diverge.
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
  FETCH_OK declaring the track's TIMESCALE, so a third-party fetch-only reader
  is timed exactly when the track is, as the untimed model requires. Since
  #4974 our own subscriber learns it from SUBSCRIBE_OK or TRACK_STATUS_OK.
  Rejected: omitting timescale from FETCH_OK and leaving those readers
  untimed.
- Honour INCLUDE_PROPERTIES (0x35) on FETCH from draft 20, once
  [Draft-20 FETCH](/quest/m1/ietf-fetch-location.md) serves draft-20 FETCH at
  all (today our publisher refuses every one). It defaults to sending the
  properties; at 0 the block is present but empty. `fetch.rs` decodes it into
  `Fetch::properties_wanted`, which the publisher does not honour yet.
- Test per draft range: FETCH_OK round-trips the properties SUBSCRIBE_OK would
  carry, a standalone FETCH's objects arrive stamped, and
  INCLUDE_PROPERTIES = 0 empties the block.

Public API: none. Wire: FETCH_OK gains its properties on drafts that define
the block. Interop: run `just test interop --all`.

## Required

- [Draft-20 FETCH](/quest/m1/ietf-fetch-location.md) - draft-20 FETCH is served at all, and edits the same `run_fetch_stream`

## Related

- [Pipelined first FETCH](/quest/m1/pipeline-requests/fetch.md) - takes a track's units from FETCH_OK properties where present
