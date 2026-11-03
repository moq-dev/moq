# [M] Announce credit

## Goal

A subscriber bounds how many routes an Announce Stream advertises to it, at
the wire: ANNOUNCE_REQUEST carries an initial route credit, the subscriber
raises it later on its side of the stream, and the publisher never holds more
live advertisements on the stream than the credit. A peer that floods distinct
paths is held to the credit instead of closing the session at the 100k
announce cap, so a flood of `.dial/*` sockets cannot drop a cluster link or the
other announcements it carries. Rust and JS implement it on the next
unpublished lite version after lite-07.

## Plan

Decided 2026-11-01 with the maintainer (the follow-up to the
[sockets](/quest/m2/socket/README.md) interview):

- Backpressure is fixed at the wire, not by a local cap. The draft already
  tells implementations to limit matching broadcasts but gives the subscriber
  no way to state a limit; its only tools are closing the stream or the
  session.
- Credit, not fastest-consumer pacing. Rust already coalesces each
  consumer's pending announcements to the latest state per path
  (`rs/moq-net/src/model/origin.rs`) and paces the outgoing stream by write
  credit, so a slow reader costs no growing buffer. Count is the unbounded
  dimension. Pausing a merged upstream stream for one slow consumer would stall
  every overlapping prefix. Credit avoids that: each Announce Stream is
  credited on its own, and a relay sets its upstream credit from its own
  budget.
- Declined: per-route refusal (frees the subscriber but not the publisher,
  and the flood still crosses the wire once), and aggregating to a covering
  prefix at the limit (loses the per-path inventory a socket `Listener`
  needs).
- The publisher withholds routes beyond the credit, coalesced to the latest
  state per path as it already does, and sends them as an ANNOUNCE_END or a
  credit raise frees room. Which withheld route goes first is the
  publisher's choice; the draft says so and Rust documents its order. A
  withheld route that is retracted is never sent. Exceeding the credit is a
  PROTOCOL_VIOLATION.
- On the new lite version, `session::Limits::announces` becomes the credit
  the session advertises, and the session never closes for count.
  moq-transport and older lite versions have no credit and keep today's
  close at the cap.
- Settle and record here how SYNCED behaves when the initial set exceeds the
  credit. For example, SYNCED could follow the credited part, and the rest
  arrives as live updates.

Update the Announce Stream, ANNOUNCE_REQUEST, and a new credit message in
`drafts/draft-lcurley-moq-lite.md`, and run `just drafts check`. Implement
both sides in `rs/moq-net` and `js/net`, gated on the version. Leave
moq-transport unchanged.

Tests on controlled time: a publisher with more routes than the credit
withholds the excess and sends it as ENDs or raises free room; a retracted
withheld route is never sent; a peer over the credit is a PROTOCOL_VIOLATION;
a relay's flooded upstream holds the credit while its other announcements keep
flowing and the session stays open; an older version still closes at the cap.

## Related

- [Announce interest memory](/quest/m2/announce-interest-memory.md) - per-stream memory proportional to what is in flight, which credit bounds
- [JS announced coalescing](/quest/m2/js-announced-coalesce.md) - the JS consumer-side queue that is still unbounded
- [Publishers learn their bitrate cap](/quest/m2/rate-grant.md) - the other wire change targeting the next lite version
