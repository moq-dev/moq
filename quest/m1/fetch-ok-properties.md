# [M] FETCH_OK carries the track's properties

## Goal

Our IETF publisher's FETCH_OK carries the same track properties its
SUBSCRIBE_OK does: max cache duration, timescale, priority, and group order.
A fetch-only reader then learns the track as a subscriber would, and a relay
meets draft 16+'s rule to include all of a track's properties.

## Plan

Found (2026-10-01) in #4647: a group FETCH with no
prior SUBSCRIBE_OK now reads its max age from FETCH_OK, but our publisher
sends an empty block.

- `rs/moq-net/src/ietf/publisher.rs` sends FETCH_OK with
  `properties: Default::default()`. SUBSCRIBE_OK fills them in the block near
  `publisher.rs:655`. Share that code so the two can't diverge.
- Our group-fetch accept path reads only the max age from FETCH_OK and
  hardcodes a microsecond timescale. Apply the same properties SUBSCRIBE_OK
  does (timescale, priority, group order) there too.
- Drafts 14-15 allow the omission: MAX_CACHE_DURATION is a MAY there, and
  FETCH_OK has no properties block. From draft 16, a relay MUST include all
  Extension Headers / Properties associated with a track in FETCH_OK (d16
  §8.6, d22 §7.7). An empty block also misleads the reader: it infers
  Ascending order and priority 128 where SUBSCRIBE_OK says Descending.
- FETCH preserves object properties, Timestamp included. Decided in the
  2026-10-05 audit (maintainer: "FETCH must send stamped objects? It's not
  legal to remove the property."): the standalone FETCH path sending its
  objects unstamped because no SUBSCRIBE declared a timescale
  (`ietf/publisher.rs`, around line 1346) is a bug to fix here. FETCH_OK declares the track's TIMESCALE, and every
  fetched object keeps its Timestamp, so a fetch-only reader is timed exactly
  when the track is, as the [untimed model](/quest/m1/untimed-model.md)
  requires. Rejected: omitting timescale from FETCH_OK and leaving fetch-only
  readers untimed.
- Honour INCLUDE_PROPERTIES (0x35) on FETCH from draft 20, once
  [Draft-20 FETCH](/quest/m1/ietf-fetch-location.md) serves draft-20 FETCH at
  all (today our publisher refuses every one). It defaults to sending the
  properties; at 0 the block is present but empty. `fetch.rs` decodes it on
  draft 20+ and drops it, so it is not honoured today.
- Test per draft range: FETCH_OK round-trips the properties SUBSCRIBE_OK would
  carry, a standalone FETCH's objects arrive stamped, and
  INCLUDE_PROPERTIES = 0 empties the block.

Public API: none. Wire: FETCH_OK gains its properties on drafts that define
the block. Interop: run `just test interop --all`.

## Required

- [Draft-20 FETCH](/quest/m1/ietf-fetch-location.md) - draft-20 FETCH is served at all, and edits the same `run_fetch_stream`

## Related

- [Fetch without SUBSCRIBE](/quest/m1/ietf-fetch-only.md) - asks TRACK_STATUS for the same properties when a publisher omits them
