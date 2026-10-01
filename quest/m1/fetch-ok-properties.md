# [S] FETCH_OK carries the track's properties

## Goal

Our IETF publisher's FETCH_OK carries the same track properties its
SUBSCRIBE_OK does: max cache duration, timescale, priority, and group order.
A fetch-only reader then learns the track as a subscriber would, and a relay
meets draft 16+'s rule to include all of a track's properties.

## Plan

Found (2026-10-01) while merging main into dev (#4647): a group FETCH with no
prior SUBSCRIBE_OK now reads its max age from FETCH_OK, but our publisher
sends an empty block.

- `rs/moq-net/src/ietf/publisher.rs` sends FETCH_OK with
  `properties: Default::default()`. SUBSCRIBE_OK fills them in the block near
  `publisher.rs:655`. Share that code so the two can't diverge.
- Drafts 14-15 allow the omission: MAX_CACHE_DURATION is a MAY there, and
  FETCH_OK has no properties block. From draft 16, a relay MUST include all
  Extension Headers / Properties associated with a track in FETCH_OK (d16
  §8.6, d22 §7.7). An empty block also misleads the reader: it infers
  Ascending order and priority 128 where SUBSCRIBE_OK says Descending.
- Honour INCLUDE_PROPERTIES (0x35) on FETCH from draft 20. It defaults to
  sending the properties; at 0 the block is present but empty. `fetch.rs`
  doesn't parse it today.
- Test per draft range: FETCH_OK round-trips the properties SUBSCRIBE_OK would
  carry, and INCLUDE_PROPERTIES = 0 empties the block.

Public API: none. Wire: FETCH_OK gains its properties on drafts that define
the block. Interop: run `just test interop --all`.

## Related

- [Fetch without SUBSCRIBE](/quest/m1/ietf-fetch-only.md) - asks TRACK_STATUS for the same properties when a publisher omits them
