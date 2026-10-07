# [M] A cold relay reports its upstream's Largest

## Goal

On drafts 14 to 19, a relay's SUBSCRIBE_OK reports the larger of its
upstream's Largest Location and the largest object it has received for the
track, including when it has cached nothing yet. A relative joining FETCH
(start 0) through a cold relay then gets the head of the current group,
filled upstream if needed, instead of INVALID_RANGE.

## Plan

Triaged from Fastly's moq-relay-interop report (2026-09-23 run, item 14)
on 2026-10-07. The report's DOES_NOT_EXIST was fixed by #4253: the empty
join is INVALID_RANGE now, which the draft requires when no Largest was sent.
What remains is that the Largest is lost. d18 §10.2.11 and d19 §10.2.16
say "A relay MUST set LARGEST_OBJECT to the largest of" the upstream's
LARGEST_OBJECT and the largest object received upstream, whatever the
cache holds; d14 to d17 state no rule, so follow the same one there.

- The publisher's SUBSCRIBE_OK carries `live_edge(&cache).largest`
  (`rs/moq-net/src/ietf/publisher.rs`, around lines 664 and 717), which is
  `None` until a group is cached, so `(Filter::NextObject, None)` becomes
  `Joined::Empty`.
- The subscriber only calls `track.start_at(largest.group)`
  (`rs/moq-net/src/ietf/subscriber.rs`, around line 2013) with its
  upstream's Largest, and does not record the object.

Decided 2026-10-07: carry the upstream's Largest into the model, so the
publisher can report the maximum of it and the cached objects, and serve the joining FETCH through the existing
one-group upstream fill. Rejected: leaving INVALID_RANGE, which is compliant
but loses late joiners' first group head. This reports state learned from
the upstream; nothing waits on a peer.

Scrutinize the model change: keep any new track state crate-private unless a
consumer needs it.

Test: through a cold relay, a d16 subscriber with a relative joining FETCH
(start 0) receives the current group from object 0, and its SUBSCRIBE_OK
carries the upstream's Largest. On d18 and d19, a relay whose cached
objects are behind the upstream's Largest reports the upstream's, and one
whose cache is ahead reports its own.

Public API: none expected. Wire: none new.

## Related

- [moq-transport ranges](/quest/m1/subscribe-ranges/ietf.md) - the same upstream fill path, for multi-group ranges
- [Fetch without SUBSCRIBE](/quest/m1/ietf-fetch-only.md) - the other FETCH gap at a relay
