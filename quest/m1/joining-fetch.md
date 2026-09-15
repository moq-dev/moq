# [M] Joining FETCH for pre-draft-20 peers

## Goal

A moq-net subscriber joins a moq-transport draft-14 to draft-19 relay at a
group boundary, with history when it asked for some: every `SUBSCRIBE` uses
the Largest Object filter and is followed by a joining `FETCH`, relative at
offset 0 for a live join and absolute at the requested group for an explicit
group-aligned and unbounded `group_start`, so the delivered groups run
contiguously from the fetched range into the live subscription. Today
`subscribe_join` collapses every pre-draft-20 subscription to
`Filter::Unfiltered`, so an explicit start never reaches the wire (an absent
filter parameter on drafts 15-19, `AbsoluteStart{0,0}` inline on draft-14),
and a live join trusts the peer to start at a group boundary, which
a peer that starts at its literal Largest Object does not.

Ranked here by maintainer decision although it is additive: it is what a
moq-net client needs against Cloudflare's relay, which speaks moqt-16, and
it should ride the dev release. Subscriber side only. Our publisher keeps
reporting Largest Object at a group boundary, so a Largest Object
subscription from it already starts whole; it serves only the current-group
prefix (`RelativeJoining` at offset 0, empty against its own subscriptions)
and refuses every other join, since a relay cannot legally answer a joining
FETCH from a partial cache and will not forward one upstream. Draft-20 and
later keep their Absolute filter and fill path.

## Plan

In `ietf::subscriber`, `subscribe_join` on a pre-draft-20 version always
yields `Filter::NextObject` plus a joining fetch, sent as its own request
after the SUBSCRIBE (the subscribe's request id names it):
`RelativeJoining { group_offset: 0 }` for `None`, `AbsoluteJoining` at
`start.group` for `Some(start)` with `start.frame == 0` and `end.is_none()`.
A frame-level start or any bounded end has no joining-FETCH spelling
(`AbsoluteJoining` names only a group, with no object or end), so those
shapes are refused rather than rounded down or left open: silently widening
them would deliver frames below the requested floor or continue live past
the requested cap.

The fill machinery is the seam to reuse, not duplicate. The relative join is
the draft-20 fill by another name: the fetch stream carries the head of the
group the subscribe stream continues, and `claim_fill` stitches it. A subscribe
stream whose first object is not 0 blocks on that head exactly as it does for a
fill; one that starts at object 0 (our own publisher's lie) stands alone and the
fetch's answer, empty or refused, is discarded. The absolute join spans whole
groups: each complete group below the subscribe's Largest Location is created,
written, and finished on the track producer, and the last one is the head the
live stream continues. Objects on these drafts carry no timestamps unless Track
Properties opted in, which the fill reader already handles.

Decisions:

- The peer refusing the FETCH (`REQUEST_ERROR`) continues the subscription
  live; the start reported is the live edge. Same outcome as today.
- A fetch stream that ends early delivers what arrived; the first delivered
  group is the start, and a hole before the live tail is a discontinuity.
- `FETCH_OK`'s end location is informational; the subscribe's Largest Location
  is what bounds the stitch.

Verification is unit tests with a scripted peer in `ietf::subscriber`: the
join is spelled correctly per draft and per start, a frame-level start or a
bounded end is refused rather than widened, a mid-group subscribe stream
waits for and stitches onto the fetched head, a whole-group stream discards the
fetch, a multi-group absolute fetch stitches into the live tail with no gap or
overlap, a refused fetch continues live, and a short fetch delivers its prefix.
Our own publisher refuses absolute joins, so it cannot be the other end of an
integration test; a Cloudflare run is opportunistic evidence, not the gate.

## Related

- [Archive](/quest/m2/archive/README.md) - the publisher-side FETCH story, served from an archive rather than the live cache
