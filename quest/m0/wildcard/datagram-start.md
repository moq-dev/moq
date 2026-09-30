# [M] Datagrams behind SUBSCRIBE_START

## Goal

On lite-07, no datagram crosses a subscription's SUBSCRIBE_START in either
direction, in Rust or JS: a publisher commits the start only for something it
actually sends, never sends a group below the start it announced, and a
subscriber delivers a datagram only after that subscription's own
SUBSCRIBE_START has named an admitted origin.

## Plan

[#4279](https://github.com/moq-dev/moq/pull/4279) made SUBSCRIBE_START
(SUBSCRIBE_OK) carry the serving origin and go out ahead of the first group
served by stream or datagram. Its last Codex round left three P1s unanswered,
and the PR auto-merged before CI. The maintainer ruled in the 09-28
merged-PR audit that all three block the line:

- **An oversized first datagram commits the start**
  ([r4117485530](https://github.com/moq-dev/moq/pull/4279#discussion_r4117485530)).
  Rust's `Recv::Datagram` arm in `rs/moq-net/src/lite/publisher.rs` calls
  `send_start` before `serve_datagram` drops a body over `max_datagram_size`, and
  JS `#runDatagrams` in `js/net/src/lite/publisher.ts` awaits
  `responses.start` before its size check. A dropped datagram at sequence 10
  can so announce a start of 10 and discard a valid group at 5. Decide
  whether a datagram can be sent before resolving the start from it.
  Since main's held first group (`TrackRun::first`), a group waits for the
  source's start (`track.poll_start`) while datagrams keep flowing with no
  START, and a datagram that goes first still resolves the start from its
  own sequence without consulting the source. Both paths should resolve the
  start the same way.
- **JS start-sequence race**
  ([r4117485532](https://github.com/moq-dev/moq/pull/4279#discussion_r4117485532)).
  `SubscribeResponses.start` reserves `#started` for whichever loop arrives
  first, but the write runs later through the `#writes` chain, so the group
  loop can pop a lower sequence in between and send group 5 after a START
  for 10. Choose the start and apply its floor atomically, or revalidate a
  group popped while the start was pending.
- **FETCH_OK admits a datagram before its own SUBSCRIBE_START**
  ([r4117485533](https://github.com/moq-dev/moq/pull/4279#discussion_r4117485533)).
  `route_datagram` in `rs/moq-net/src/lite/subscriber.rs` gates only on the
  shared track `Provenance` being admitted. A FETCH on the same track can
  admit it through FETCH_OK while this subscription has not started, so a
  racing datagram is delivered, and a later START naming another origin is
  caught only after content was exposed. Require this subscription's own
  start as well. Check whether the JS subscriber has the same gap.

Each fix gets a regression test that fails without it, in the language it
touches.

## Related

- [Wildcard](/quest/m0/wildcard/README.md) - the line this blocks
